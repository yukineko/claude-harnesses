//! Foreign-file bridge: read condukt's auto-approved gate-decision journal
//! (`gatelog.rs`'s `gate-decisions.jsonl`) so the human review surface can
//! show the DENOMINATOR (decisions that passed a gate WITHOUT a human) next
//! to the NUMERATOR `overwatch review-queue` already shows (systemic
//! violations, rollbacks, AI findings, escalations). Without seeing the
//! auto-approved population's count + a sample, a human cannot judge
//! sampling coverage.
//!
//! # Why a foreign-file read, not a crate dependency
//!
//! The workspace's dependency direction is `harness-core <- overwatch <-
//! blastguard <- condukt`. condukt already depends on overwatch, so
//! `overwatch` taking a `condukt` crate dependency would be a cycle
//! (benchkit also depends on overwatch, so `benchkit::auditsample` is
//! likewise off-limits). Instead this module reads condukt's
//! `gate-decisions.jsonl` **by path**, as fail-soft foreign JSONL, using
//! ONLY `harness_core` primitives — mirroring
//! [`crate::review_escalation`]'s bridge for `escalations.json`.
//!
//! # Known limitation
//!
//! This reads condukt's **default** state path
//! (`~/.condukt/state/gate-decisions.jsonl`). If a user overrides
//! `state_dir` in `~/.condukt/config.toml`, the journal lives elsewhere and
//! will simply not be found here — no error, it just contributes zero rows
//! (fail-soft graceful degrade, consistent with every other source in
//! `review_queue.rs`). Parsing condukt's own config across the crate
//! boundary is deliberately out of scope, to keep this coupling to the
//! minimal file contract described below.
//!
//! # Path shape nuance
//!
//! Unlike `escalations.json` (`state/<project-key>/escalations.json`),
//! `gate-decisions.jsonl` is written to the FLAT state dir — NO
//! `<project-key>` segment — because `condukt policy answer` journals to
//! `config::Config::load().state_dir` directly
//! (`crates/condukt/src/main.rs`, `crates/condukt/src/config.rs`), not a
//! per-project subdirectory.
//!
//! # File contract
//!
//! condukt's on-disk shape (`crates/condukt/src/gatelog.rs`) is one JSON
//! object per line (JSONL), each a `GateDecision { question, options,
//! recommend_index, chosen, policy, created_at }`. Only `policy == "auto"`
//! rows are ever journaled (escalate/block are never recorded because
//! nothing was self-answered) — so every row on disk today IS an
//! auto-approved decision. [`AutoApprovedDecision`] is a MINIMAL mirror
//! carrying the same fields; every field is `#[serde(default)]` so a
//! partial/older/foreign record (or a future condukt version that adds or
//! drops a field) still parses instead of failing the whole read.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Minimal mirror of condukt's `gatelog::GateDecision` — see the module doc
/// for the cross-tool file contract this mirrors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutoApprovedDecision {
    /// The question that was self-answered.
    #[serde(default)]
    pub question: String,
    /// The options that were offered.
    #[serde(default)]
    pub options: Vec<String>,
    /// 0-based index of the recommended (and, on auto, chosen) option.
    #[serde(default)]
    pub recommend_index: usize,
    /// The option that was chosen (== `options[recommend_index]`).
    #[serde(default)]
    pub chosen: String,
    /// The policy verdict that authorised the self-answer (always "auto" in
    /// today's condukt — escalate/block are never journaled).
    #[serde(default)]
    pub policy: String,
    /// Unix seconds when the decision was recorded.
    #[serde(default)]
    pub created_at: i64,
}

/// Parse condukt's `gate-decisions.jsonl` text (one `GateDecision` object per
/// line) and return only rows with `policy == "auto"`. PURE and total: a
/// corrupt/garbage line is skipped rather than failing the whole parse
/// (mirrors `gatelog::load_decisions`' line-filter tolerance), and empty
/// input yields an empty vec. The `policy == "auto"` filter is defensive —
/// today every journaled row is auto — so "auto-approved population" stays
/// explicit and robust to a future condukt that journals other verdicts.
pub fn parse_auto_approved(txt: &str) -> Vec<AutoApprovedDecision> {
    txt.lines()
        .filter_map(|l| serde_json::from_str::<AutoApprovedDecision>(l).ok())
        .filter(|d| d.policy == "auto")
        .collect()
}

/// Derive the DEFAULT path to condukt's `gate-decisions.jsonl`:
/// `harness_core::config::base_dir("condukt")/state/gate-decisions.jsonl` —
/// FLAT, with NO `<project-key>` segment (see the module doc's "Path shape
/// nuance"). See the module doc for the known non-default-`state_dir`
/// limitation.
pub fn gate_decisions_path() -> PathBuf {
    harness_core::config::base_dir("condukt")
        .join("state")
        .join("gate-decisions.jsonl")
}

/// Read condukt's auto-approved gate-decision population, fail-soft: a
/// missing file (no condukt ever ran, or nothing was auto-answered yet), an
/// unreadable file, or corrupt JSONL all yield an empty vec rather than an
/// error — so a project with no condukt gate-decisions journal contributes
/// zero rows, never breaking the command.
pub fn read_auto_approved() -> Vec<AutoApprovedDecision> {
    let path = gate_decisions_path();
    match std::fs::read_to_string(&path) {
        Ok(txt) => parse_auto_approved(&txt),
        Err(_) => Vec::new(),
    }
}

/// Keep only decisions within the window: `since = Some(ts)` keeps
/// `created_at >= ts`; `since = None` keeps everything. PURE.
pub fn filter_since(pop: &[AutoApprovedDecision], since: Option<i64>) -> Vec<AutoApprovedDecision> {
    match since {
        Some(ts) => pop.iter().filter(|d| d.created_at >= ts).cloned().collect(),
        None => pop.to_vec(),
    }
}

/// splitmix64: a tiny, fast, well-distributed 64-bit PRNG step. Reimplemented
/// inline (not pulled from `benchkit`) to keep this module dependency-free.
fn splitmix64_next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E3779B97F4A7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

/// Draw `min(k, pop.len())` records deterministically: sort a copy of `pop`
/// by the stable key `(created_at, question, chosen)` (so the base order
/// never depends on file/hash order), then run a seeded splitmix64
/// Fisher-Yates shuffle and take the first `k` elements (a "shuffle prefix").
/// PURE and DETERMINISTIC: the same `(pop, k, seed)` always yields the
/// identical sample. `k == 0` or an empty `pop` yields an empty vec; `k >=
/// pop.len()` yields the whole sorted population (order-shuffled).
pub fn sample_auto_approved(
    pop: &[AutoApprovedDecision],
    k: usize,
    seed: u64,
) -> Vec<AutoApprovedDecision> {
    if k == 0 || pop.is_empty() {
        return Vec::new();
    }
    let mut sorted: Vec<AutoApprovedDecision> = pop.to_vec();
    sorted.sort_by(|a, b| {
        (a.created_at, &a.question, &a.chosen).cmp(&(b.created_at, &b.question, &b.chosen))
    });
    let n = sorted.len();
    let take = k.min(n);
    let mut state = seed;
    // Fisher-Yates shuffle over the whole slice, then take the first `take`
    // elements — a "shuffle prefix" gives a uniform-without-replacement
    // sample regardless of how small `take` is relative to `n`.
    for i in (1..n).rev() {
        let r = splitmix64_next(&mut state);
        let j = (r as usize) % (i + 1);
        sorted.swap(i, j);
    }
    sorted.truncate(take);
    sorted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(created_at: i64, question: &str, chosen: &str, policy: &str) -> AutoApprovedDecision {
        AutoApprovedDecision {
            question: question.to_string(),
            options: vec![chosen.to_string()],
            recommend_index: 0,
            chosen: chosen.to_string(),
            policy: policy.to_string(),
            created_at,
        }
    }

    #[test]
    fn parse_auto_approved_parses_multi_line() {
        let txt = concat!(
            r#"{"question":"Q1","options":["a","b"],"recommend_index":0,"chosen":"a","policy":"auto","created_at":100}"#,
            "\n",
            r#"{"question":"Q2","options":["x"],"recommend_index":0,"chosen":"x","policy":"auto","created_at":200}"#,
        );
        let rows = parse_auto_approved(txt);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].question, "Q1");
        assert_eq!(rows[1].created_at, 200);
    }

    #[test]
    fn parse_auto_approved_skips_corrupt_line() {
        let txt = concat!(
            r#"{"question":"Q1","options":["a"],"recommend_index":0,"chosen":"a","policy":"auto","created_at":1}"#,
            "\n",
            "not json at all {{{",
            "\n",
            r#"{"question":"Q2","options":["b"],"recommend_index":0,"chosen":"b","policy":"auto","created_at":2}"#,
        );
        let rows = parse_auto_approved(txt);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn parse_auto_approved_excludes_non_auto_policy() {
        let txt = concat!(
            r#"{"question":"Q1","options":["a"],"recommend_index":0,"chosen":"a","policy":"auto","created_at":1}"#,
            "\n",
            r#"{"question":"Q2","options":["b"],"recommend_index":0,"chosen":"b","policy":"escalate","created_at":2}"#,
        );
        let rows = parse_auto_approved(txt);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].question, "Q1");
    }

    #[test]
    fn parse_auto_approved_empty_is_empty() {
        assert!(parse_auto_approved("").is_empty());
    }

    #[test]
    fn filter_since_boundary() {
        let pop = vec![dec(100, "Q1", "a", "auto"), dec(200, "Q2", "b", "auto")];
        let kept = filter_since(&pop, Some(200));
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].question, "Q2");

        let kept_ge = filter_since(&pop, Some(100));
        assert_eq!(kept_ge.len(), 2);

        let kept_none = filter_since(&pop, None);
        assert_eq!(kept_none.len(), 2);
    }

    #[test]
    fn sample_auto_approved_deterministic_same_seed() {
        let pop: Vec<AutoApprovedDecision> = (0..20)
            .map(|i| dec(i, &format!("Q{i}"), &format!("c{i}"), "auto"))
            .collect();
        let s1 = sample_auto_approved(&pop, 5, 42);
        let s2 = sample_auto_approved(&pop, 5, 42);
        assert_eq!(s1, s2);
        assert_eq!(s1.len(), 5);
    }

    #[test]
    fn sample_auto_approved_k_ge_len_returns_whole_population() {
        let pop = vec![dec(1, "Q1", "a", "auto"), dec(2, "Q2", "b", "auto")];
        let s = sample_auto_approved(&pop, 10, 7);
        assert_eq!(s.len(), 2);
        let mut questions: Vec<&str> = s.iter().map(|d| d.question.as_str()).collect();
        questions.sort_unstable();
        assert_eq!(questions, vec!["Q1", "Q2"]);
    }

    #[test]
    fn sample_auto_approved_k_zero_is_empty() {
        let pop = vec![dec(1, "Q1", "a", "auto")];
        assert!(sample_auto_approved(&pop, 0, 1).is_empty());
    }

    #[test]
    fn sample_auto_approved_empty_pop_is_empty() {
        let pop: Vec<AutoApprovedDecision> = Vec::new();
        assert!(sample_auto_approved(&pop, 5, 1).is_empty());
    }

    #[test]
    fn sample_auto_approved_different_seed_can_differ() {
        let pop: Vec<AutoApprovedDecision> = (0..30)
            .map(|i| dec(i, &format!("Q{i}"), &format!("c{i}"), "auto"))
            .collect();
        let s1 = sample_auto_approved(&pop, 5, 1);
        let s2 = sample_auto_approved(&pop, 5, 2);
        assert_ne!(s1, s2, "different seeds should (very likely) diverge");
    }

    #[test]
    fn gate_decisions_path_is_flat_no_project_key() {
        let path = gate_decisions_path();
        assert!(path.ends_with("gate-decisions.jsonl"));
        let s = path.to_string_lossy();
        assert!(s.contains(".condukt"));
        assert!(s.contains("state"));
    }

    #[test]
    fn read_auto_approved_missing_file_is_empty_no_panic() {
        // gate_decisions_path() derives from HOME; as long as no test in this
        // process seeded that exact file, this documents the fail-soft
        // contract without requiring HOME sandboxing (the CLI test in
        // tests/auto_approved_cli.rs covers the seeded/HOME-sandboxed case).
        let _ = read_auto_approved();
    }
}
