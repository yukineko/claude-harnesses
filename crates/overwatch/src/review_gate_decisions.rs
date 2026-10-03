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
//! `gate-decisions.jsonl` **by path**, as foreign JSONL whose absence is
//! tolerated but whose unreadability is not (see below), using
//! ONLY `harness_core` primitives — mirroring
//! [`crate::review_escalation`]'s bridge for `escalations.json`.
//!
//! # Known limitation
//!
//! This reads condukt's **default** state path
//! (`~/.condukt/state/gate-decisions.jsonl`). If a user overrides
//! `state_dir` in `~/.condukt/config.toml`, the journal lives elsewhere and
//! will simply not be found here — an ABSENT journal is a determined
//! observation and does contribute zero rows. Parsing condukt's own config
//! across the crate boundary is deliberately out of scope, to keep this
//! coupling to the minimal file contract described below.
//!
//! What is NOT fail-soft any more is a journal that exists and could not be
//! read, or that decoded only partially: those are `Undetermined`, not zero
//! (backlog afdcfd4d). This module is the audit surface for decisions that
//! passed a gate WITHOUT a human, so "0 decisions passed without review" is
//! the single most reassuring sentence it can print — and it must never be
//! printed on the strength of a read that did not happen. Absent stays zero;
//! unreadable and undecodable stop being zero.
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
//! recommend_index, chosen, policy, created_at }`.
//!
//! **condukt journals every resolved verdict**, so a row's `policy` is
//! `"auto"`, `"escalate"` or `"block"` and NOT every row is auto-approved.
//! (It journaled `auto` alone until 2026-09-07; recording only self-answers
//! made an escalated gate indistinguishable from a gate that never fired.)
//! This module's denominator is the auto-approved population specifically, so
//! [`parse_auto_approved`] keeps `policy == "auto"` rows only — that filter
//! was already written defensively for exactly this change and needed no
//! adjustment when it landed. An `escalate`/`block` row carries
//! `"chosen": null`, which is why [`AutoApprovedDecision::chosen`] is an
//! `Option<String>`: with a bare `String` such a row fails to deserialize and
//! is silently dropped by the line filter, which happens to give the right
//! count here but hides the row for the wrong reason.
//!
//! [`AutoApprovedDecision`] is a MINIMAL mirror carrying the same fields;
//! every field is `#[serde(default)]` so a partial/older/foreign record (or a
//! future condukt version that adds or drops a field) still parses instead of
//! failing the whole read.

use harness_core::verdict::Determination;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
    /// The option that was chosen (== `options[recommend_index]`), or `None`
    /// on a row where the policy answered nothing. Rows reaching this struct
    /// after [`parse_auto_approved`] are always `Some` (the filter keeps only
    /// `policy == "auto"`); the `Option` exists so an `escalate`/`block` row's
    /// `"chosen": null` DESERIALIZES and is then filtered out deliberately,
    /// rather than failing to parse and vanishing via the line filter.
    #[serde(default)]
    pub chosen: Option<String>,
    /// The policy verdict this row records: `"auto"`, `"escalate"` or
    /// `"block"`. Only `"auto"` survives [`parse_auto_approved`].
    #[serde(default)]
    pub policy: String,
    /// Unix seconds when the decision was recorded.
    #[serde(default)]
    pub created_at: i64,
}

/// Parse condukt's `gate-decisions.jsonl` text (one `GateDecision` object per
/// line), returning the `policy == "auto"` rows AND why the text could not be
/// decoded in full, if it could not.
///
/// PURE and total. The second element mirrors `store::decode_jsonl_lines`'
/// contract: `None` means every non-blank line decoded, `Some(why)` names the
/// first line that did not. A dropped line used to be invisible, which made a
/// PARTIAL population indistinguishable from a complete one — and this
/// module's output is a denominator, so under-reporting it overstates how
/// well the auto-approved population is being sampled. The caller resolves
/// `Some(why)` to `Undetermined`; the rows are still returned so a caller that
/// wants the partial set can have it deliberately rather than by accident.
///
/// Blank lines are not a decode failure (a trailing newline is normal JSONL).
///
/// The `policy == "auto"` filter is load-bearing, not decorative: condukt
/// journals `escalate` and `block` rows too, and those went TO a human, so
/// counting them in the auto-approved denominator would overstate how much
/// passed without one. It was written defensively before condukt journaled
/// anything but `auto`, which is why it needed no change when condukt started
/// (2026-09-07). A row filtered out by POLICY is not an undecodable row: it
/// decoded fine and was then deliberately excluded.
pub fn parse_auto_approved(txt: &str) -> (Vec<AutoApprovedDecision>, Option<String>) {
    let mut rows = Vec::new();
    let mut undecodable: Option<String> = None;
    for (i, line) in txt.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<AutoApprovedDecision>(line) {
            Ok(d) => {
                if d.policy == "auto" {
                    rows.push(d);
                }
            }
            Err(e) => {
                if undecodable.is_none() {
                    undecodable = Some(format!(
                        "gate-decisions.jsonl line {} did not decode: {e}",
                        i + 1
                    ));
                }
            }
        }
    }
    (rows, undecodable)
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

/// Read the auto-approved population from `path`, three-valued.
///
/// * absent — `Known(vec![])`. Nothing was ever journaled; that IS zero, and
///   it is the common case on a machine where condukt has never self-answered.
/// * read and fully decoded — `Known(rows)`.
/// * present but unreadable — `Undetermined`, forwarded from
///   `harness_core::boundary::read_to_string` rather than re-minted, so the
///   boundary's reason reaches the operator intact.
/// * decoded only in part — `Undetermined`. The rows are discarded rather than
///   returned as a count, because a count is exactly what a reader would trust.
///
/// Split out from [`read_auto_approved`] so the behaviour is reachable without
/// the process-global `$HOME` this module's default path resolves through.
pub fn read_auto_approved_at(path: &Path) -> Determination<Vec<AutoApprovedDecision>> {
    match harness_core::boundary::read_to_string(path) {
        Determination::Known(None) => Determination::known(Vec::new()),
        Determination::Known(Some(txt)) => match parse_auto_approved(&txt) {
            (rows, None) => Determination::known(rows),
            (_, Some(why)) => Determination::undetermined(why),
        },
        // Forwarded, deliberately not re-minted: the boundary already recorded
        // this `Undetermined` once (same reasoning as `store::scan_jsonl`).
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

/// Read condukt's auto-approved gate-decision population from the default
/// path. Thin wrapper over [`read_auto_approved_at`]; see that function for
/// the three-valued contract.
pub fn read_auto_approved() -> Determination<Vec<AutoApprovedDecision>> {
    read_auto_approved_at(&gate_decisions_path())
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
            chosen: Some(chosen.to_string()),
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
        let (rows, undecodable) = parse_auto_approved(txt);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].question, "Q1");
        assert_eq!(rows[1].created_at, 200);
        assert_eq!(undecodable, None, "both lines are valid JSON");
    }

    /// Renamed and STRENGTHENED (backlog afdcfd4d). It used to be called
    /// `parse_auto_approved_skips_corrupt_line` and asserted only
    /// `rows.len() == 2`, i.e. it wrote the silent drop down as the contract --
    /// the CLAUDE.md 2 failure mode where an unexamined assert becomes the
    /// specification. Nothing is relaxed here: the good rows must still come
    /// back. What is ADDED is that the line which did not decode is reported,
    /// so a caller cannot mistake a partial population for a complete one.
    #[test]
    fn parse_auto_approved_reports_the_line_it_could_not_decode() {
        let txt = concat!(
            r#"{"question":"Q1","options":["a"],"recommend_index":0,"chosen":"a","policy":"auto","created_at":1}"#,
            "\n",
            "not json at all {{{",
            "\n",
            r#"{"question":"Q2","options":["b"],"recommend_index":0,"chosen":"b","policy":"auto","created_at":2}"#,
        );
        let (rows, undecodable) = parse_auto_approved(txt);
        assert_eq!(rows.len(), 2, "the two decodable rows are still returned");
        let why = undecodable.expect(
            "the garbage line must be reported; dropping it silently is what made \
             a partial population indistinguishable from a complete one",
        );
        assert!(
            why.contains("line 2"),
            "the report must name WHICH line failed, so it can be found: {why}"
        );
    }

    #[test]
    fn parse_auto_approved_excludes_non_auto_policy() {
        let txt = concat!(
            r#"{"question":"Q1","options":["a"],"recommend_index":0,"chosen":"a","policy":"auto","created_at":1}"#,
            "\n",
            r#"{"question":"Q2","options":["b"],"recommend_index":0,"chosen":"b","policy":"escalate","created_at":2}"#,
        );
        let (rows, undecodable) = parse_auto_approved(txt);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].question, "Q1");
        assert_eq!(
            undecodable, None,
            "a row excluded by POLICY decoded fine; it is not an undecodable line"
        );
    }

    #[test]
    fn parse_auto_approved_empty_is_empty() {
        let (rows, undecodable) = parse_auto_approved("");
        assert!(rows.is_empty());
        assert_eq!(undecodable, None, "no lines means no line failed to decode");
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

    /// Was `read_auto_approved_missing_file_is_empty_no_panic`, whose whole
    /// body was `let _ = read_auto_approved();` -- no assertion, so it could
    /// not fail and proved nothing (CLAUDE.md 2b). It also went through the
    /// process-global HOME, so what it exercised depended on the machine.
    /// Now it names the path and pins the answer: ABSENT is a determined
    /// observation of zero, and must stay distinguishable from the unreadable
    /// and part-decoded cases, which are `Undetermined`.
    #[test]
    fn an_absent_journal_is_a_known_zero_not_undetermined() {
        let path = std::env::temp_dir().join(format!(
            "overwatch-afdcfd4d-absent-{}-{}.jsonl",
            std::process::id(),
            line!()
        ));
        assert!(!path.exists(), "apparatus: the path must not exist");
        let got = read_auto_approved_at(&path);
        let rendered = match &got {
            Determination::Known(rows) => format!("Known({} rows)", rows.len()),
            Determination::Undetermined(why) => format!("Undetermined({})", why.as_str()),
        };
        assert!(
            matches!(&got, Determination::Known(rows) if rows.is_empty()),
            "an absent journal is nothing-was-journaled, which IS zero; reporting it \
             undetermined would turn the common case into noise. got: {rendered}"
        );
    }
}
