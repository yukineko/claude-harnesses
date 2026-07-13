//! Deterministic review-WORTHINESS (review-COST) scoring — a signal distinct
//! from `blastguard`/`diffrisk`'s blast-radius (downstream impact of a
//! change).
//!
//! Blast-radius asks "how much could this change break, downstream?".
//! Review-worthiness asks a DIFFERENT question, judged purely from the
//! change's OWN shape: "how much human review attention does this change
//! WARRANT?". A tiny, well-documented, task-linked change warrants little
//! attention even if (hypothetically) it touched a sensitive path; a huge,
//! rationale-free, untracked, mostly-deletions change warrants a lot even if
//! it touches nothing blastguard considers sensitive. The two signals are
//! meant to be consulted TOGETHER by a future review-budget allocator, never
//! folded into one another.
//!
//! # Honest-scope note
//!
//! This module is intentionally kept SEPARATE from
//! [`crate::review_brief::build_review_brief`], which is documented as
//! composed ENTIRELY from static, already-persisted, declared signals with
//! NO live git diff. `score_review_worthiness` reads live diff SHAPE
//! (insertions/deletions/files, commit rationale, task-link presence) — a
//! diff-reading heuristic that would violate `review_brief`'s no-live-diff
//! purity contract if folded in. It is its own CLI (`condukt
//! review-worthiness`) and its own future consumer (a review-budget
//! allocator), not a `review_brief` field.
//!
//! # Determinism
//!
//! [`score_review_worthiness`], [`parse_numstat`], [`has_rationale`], and
//! [`has_task_link`] are all PURE: no I/O, no wall-clock, no randomness. All
//! git shell-out (the `--from-git` CLI convenience mode) lives at the CLI
//! boundary in `main.rs`, fail-soft (a git failure degrades to zeros/false,
//! never an error).

use serde::{Deserialize, Serialize};

/// The declared/measured shape of a change, as fed to
/// [`score_review_worthiness`]. Deliberately flat and IO-free so it can be
/// built either from CLI flags (hermetic, the tested contract) or from a
/// live `git diff --numstat` + `git log` gather (the `--from-git`
/// convenience mode, fail-soft at the CLI boundary).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewWorthinessInputs {
    /// Number of files touched by the change. Captured for context/echo;
    /// the current penalty formulas below score total changed LINES, not
    /// file count (see [`score_review_worthiness`] doc for the exact terms
    /// scored today).
    pub files_changed: u32,
    /// Total inserted lines across the change.
    pub insertions: u64,
    /// Total deleted lines across the change.
    pub deletions: u64,
    /// Whether the change carries a non-empty rationale (see
    /// [`has_rationale`]).
    pub has_rationale: bool,
    /// Whether the change is linked to a tracked task/backlog id (see
    /// [`has_task_link`]).
    pub has_task_link: bool,
}

/// The scored result: a bounded integer `score` plus one human-readable
/// `drivers` entry per penalty TERM that actually contributed (i.e. whose
/// individual penalty is `> 0`) — so the output explains itself without a
/// second lookup. A change with a `score` of `0` has an EMPTY `drivers`
/// list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewWorthiness {
    pub score: u32,
    pub drivers: Vec<String>,
}

/// Size-penalty scale: one point per this many total changed lines
/// (`insertions + deletions`), floor-divided (integer division — e.g. 19
/// changed lines contribute `0`, 20 contribute `1`).
pub const SIZE_PENALTY_LINES_PER_POINT: u64 = 20;
/// Size-penalty ceiling: a change can never contribute more than this many
/// points via sheer size, however large.
pub const SIZE_PENALTY_CAP: u32 = 40;

/// Net-deletion-penalty scale: one point per this many NET deleted lines
/// (`max(0, deletions - insertions)`), floor-divided.
pub const NET_DELETION_LINES_PER_POINT: u64 = 10;
/// Net-deletion-penalty ceiling: a change can never contribute more than
/// this many points via net deletion, however lopsided.
pub const NET_DELETION_PENALTY_CAP: u32 = 30;

/// Fixed penalty applied when [`ReviewWorthinessInputs::has_rationale`] is
/// `false` (the change cannot self-explain via its commit body).
pub const MISSING_RATIONALE_PENALTY: u32 = 15;
/// Fixed penalty applied when [`ReviewWorthinessInputs::has_task_link`] is
/// `false` (the change is not linked to any tracked task/backlog id).
pub const ABSENT_TASK_LINK_PENALTY: u32 = 15;

/// Score a change's review-worthiness (review-cost) from its own shape.
///
/// PURE: no I/O, no wall-clock, no randomness — the same inputs always
/// produce the same [`ReviewWorthiness`]. Sums four independently-bounded
/// penalty terms into `score` (max possible score today: `40 + 30 + 15 + 15
/// = 100`), pushing one human-readable `drivers` entry per term whose
/// individual penalty is `> 0`:
///
/// 1. **size penalty** — `min(SIZE_PENALTY_CAP, (insertions + deletions) /
///    SIZE_PENALTY_LINES_PER_POINT)`: one point per
///    [`SIZE_PENALTY_LINES_PER_POINT`] total changed lines, capped at
///    [`SIZE_PENALTY_CAP`].
/// 2. **net-deletion penalty** — `min(NET_DELETION_PENALTY_CAP,
///    max(0, deletions - insertions) / NET_DELETION_LINES_PER_POINT)`: one
///    point per [`NET_DELETION_LINES_PER_POINT`] lines of NET deletion
///    (deletions exceeding insertions), capped at
///    [`NET_DELETION_PENALTY_CAP`]. A change with more insertions than
///    deletions contributes `0` here (net deletion floors at zero, never
///    negative).
/// 3. **missing-rationale penalty** — a fixed [`MISSING_RATIONALE_PENALTY`]
///    when `!inputs.has_rationale`, else `0`.
/// 4. **absent-task-link penalty** — a fixed [`ABSENT_TASK_LINK_PENALTY`]
///    when `!inputs.has_task_link`, else `0`.
pub fn score_review_worthiness(inputs: &ReviewWorthinessInputs) -> ReviewWorthiness {
    let mut score: u32 = 0;
    let mut drivers = Vec::new();

    let total_changed = inputs.insertions.saturating_add(inputs.deletions);
    // Cap in u64 BEFORE casting down to u32, so a pathologically large
    // total_changed can never wrap around u32::MAX before the min() clamps
    // it to the documented cap.
    let size_penalty =
        (total_changed / SIZE_PENALTY_LINES_PER_POINT).min(SIZE_PENALTY_CAP as u64) as u32;
    if size_penalty > 0 {
        score += size_penalty;
        drivers.push(format!(
            "size: {total_changed} total changed lines (+{}/-{}) adds {size_penalty} review-cost points (cap {SIZE_PENALTY_CAP})",
            inputs.insertions, inputs.deletions
        ));
    }

    let net_deletion = inputs.deletions.saturating_sub(inputs.insertions);
    let net_deletion_penalty =
        (net_deletion / NET_DELETION_LINES_PER_POINT).min(NET_DELETION_PENALTY_CAP as u64) as u32;
    if net_deletion_penalty > 0 {
        score += net_deletion_penalty;
        drivers.push(format!(
            "net-deletion: {net_deletion} more lines removed than added adds {net_deletion_penalty} review-cost points (cap {NET_DELETION_PENALTY_CAP})"
        ));
    }

    if !inputs.has_rationale {
        score += MISSING_RATIONALE_PENALTY;
        drivers.push(format!(
            "missing-rationale: empty commit rationale adds {MISSING_RATIONALE_PENALTY} review-cost points"
        ));
    }

    if !inputs.has_task_link {
        score += ABSENT_TASK_LINK_PENALTY;
        drivers.push(format!(
            "absent-task-link: no tracked task/backlog id found adds {ABSENT_TASK_LINK_PENALTY} review-cost points"
        ));
    }

    ReviewWorthiness { score, drivers }
}

/// Aggregate totals parsed from `git diff --numstat` output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NumstatTotals {
    pub files_changed: u32,
    pub insertions: u64,
    pub deletions: u64,
}

/// PURE parser for `git diff --numstat` text: each line is
/// `<insertions>\t<deletions>\t<path>`, where either count may be a literal
/// `-` (git's marker for a binary/uncountable file), treated as `0`. Sums
/// insertions/deletions across all well-formed lines and counts one file per
/// well-formed line (a binary-dash line still counts as ONE changed file,
/// just with zero countable lines).
///
/// Fail-soft: any line that does not split into exactly three
/// tab-separated fields with a non-empty path, or whose non-dash count
/// field fails to parse as a `u64`, is SKIPPED (never panics, never
/// aborts the whole parse — one garbage line doesn't poison the rest).
/// Empty input yields all-zero totals.
pub fn parse_numstat(text: &str) -> NumstatTotals {
    let mut totals = NumstatTotals::default();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, '\t');
        let (Some(ins_s), Some(del_s), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue; // malformed: fewer than 3 tab-separated fields
        };
        if path.trim().is_empty() {
            continue; // malformed: empty path
        }

        let ins = match parse_numstat_count(ins_s) {
            Some(v) => v,
            None => continue, // malformed count: skip the whole line
        };
        let del = match parse_numstat_count(del_s) {
            Some(v) => v,
            None => continue,
        };

        totals.insertions = totals.insertions.saturating_add(ins);
        totals.deletions = totals.deletions.saturating_add(del);
        totals.files_changed = totals.files_changed.saturating_add(1);
    }

    totals
}

/// One numstat count field: `-` (binary/uncountable) parses as `Some(0)`;
/// anything else parses as a `u64` or fails (`None`, so the caller can skip
/// the malformed line fail-soft).
fn parse_numstat_count(field: &str) -> Option<u64> {
    let field = field.trim();
    if field == "-" {
        Some(0)
    } else {
        field.parse::<u64>().ok()
    }
}

/// A change has a rationale iff its commit BODY (everything after the
/// summary line — the caller passes just the body) is non-empty after
/// trimming whitespace. Pure.
pub fn has_rationale(commit_body: &str) -> bool {
    !commit_body.trim().is_empty()
}

/// A change is task-linked iff its commit message contains a tracked-task
/// token. Exactly two patterns count, matched case-insensitively:
///
/// 1. The literal word `backlog` followed (after optional whitespace/`:`/
///    `#`/`-`/`/`) by an alphanumeric id of at least 4 characters, e.g.
///    `backlog: c3dcbd6d`, `backlog #1234abcd`, `Backlog/task42`.
/// 2. A `run-` prefix followed by exactly 8 digits, e.g. `run-20260713` (the
///    date stem of a condukt run id such as `run-20260713-012935`). The
///    8-digit anchor is deliberate: it matches a run-id date stem but NOT
///    prose like `run-time` or `run-down`, since neither is 8 digits.
///
/// A BARE standalone 8-hex-character token (e.g. `c3dcbd6d`) is
/// intentionally NOT treated as a task link, even though it is this repo's
/// backlog id shape: it is lexically indistinguishable from an 8-char git
/// short-SHA (`a1b2c3d4`, `deadbeef`), so a commit that only cites a SHA
/// (e.g. "revert of a1b2c3d4") must not be mis-scored as task-linked. The
/// same hex IS accepted when it carries explicit context, e.g.
/// `(backlog c3dcbd6d)`.
///
/// Pure (compiles a fresh, statically-correct regex per call — this is a
/// low-frequency CLI-boundary check, not a hot loop).
pub fn has_task_link(commit_message: &str) -> bool {
    let re = regex::Regex::new(r"(?i)backlog[\s:#/-]*[0-9a-z]{4,}|\brun-[0-9]{8}")
        .expect("static task-link regex is valid");
    re.is_match(commit_message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(
        files_changed: u32,
        insertions: u64,
        deletions: u64,
        has_rationale: bool,
        has_task_link: bool,
    ) -> ReviewWorthinessInputs {
        ReviewWorthinessInputs {
            files_changed,
            insertions,
            deletions,
            has_rationale,
            has_task_link,
        }
    }

    // --- score_review_worthiness: the RED->GREEN feature proof ---

    #[test]
    fn high_worthiness_large_net_deletion_no_rationale_no_task_link_scores_high_all_drivers() {
        // total_changed = 510 -> size penalty = 510/20 = 25 (below cap 40)
        // net_deletion = 500-10 = 490 -> 490/10 = 49 -> capped at 30
        // missing rationale + absent task link -> 15 + 15
        // total score = 25 + 30 + 15 + 15 = 85
        let out = score_review_worthiness(&inputs(20, 10, 500, false, false));
        assert_eq!(out.score, 85);
        assert_eq!(out.drivers.len(), 4);
        assert!(out.drivers[0].starts_with("size:"));
        assert!(out.drivers[1].starts_with("net-deletion:"));
        assert!(out.drivers[2].starts_with("missing-rationale:"));
        assert!(out.drivers[3].starts_with("absent-task-link:"));
    }

    #[test]
    fn low_worthiness_small_documented_task_linked_change_scores_zero_no_drivers() {
        // total_changed = 8 -> 8/20 = 0
        // deletions(3) < insertions(5) -> net deletion floors at 0
        // rationale present, task link present -> no fixed penalties
        let out = score_review_worthiness(&inputs(1, 5, 3, true, true));
        assert_eq!(out.score, 0);
        assert!(out.drivers.is_empty());
    }

    #[test]
    fn size_penalty_is_capped() {
        let out = score_review_worthiness(&inputs(5, 10_000, 10_000, true, true));
        assert_eq!(out.score, SIZE_PENALTY_CAP);
        assert_eq!(out.drivers.len(), 1);
    }

    #[test]
    fn net_deletion_penalty_is_capped() {
        let out = score_review_worthiness(&inputs(1, 0, 100_000, true, true));
        // size: total_changed=100000 -> 100000/20=5000 -> capped 40
        // net_deletion: 100000/10=10000 -> capped 30
        assert_eq!(out.score, SIZE_PENALTY_CAP + NET_DELETION_PENALTY_CAP);
    }

    #[test]
    fn missing_rationale_alone_contributes_fixed_penalty() {
        let out = score_review_worthiness(&inputs(1, 0, 0, false, true));
        assert_eq!(out.score, MISSING_RATIONALE_PENALTY);
        assert_eq!(out.drivers.len(), 1);
        assert!(out.drivers[0].starts_with("missing-rationale:"));
    }

    #[test]
    fn absent_task_link_alone_contributes_fixed_penalty() {
        let out = score_review_worthiness(&inputs(1, 0, 0, true, false));
        assert_eq!(out.score, ABSENT_TASK_LINK_PENALTY);
        assert_eq!(out.drivers.len(), 1);
        assert!(out.drivers[0].starts_with("absent-task-link:"));
    }

    #[test]
    fn all_zero_inputs_with_rationale_and_link_scores_zero() {
        let out = score_review_worthiness(&inputs(0, 0, 0, true, true));
        assert_eq!(out.score, 0);
        assert!(out.drivers.is_empty());
    }

    // --- parse_numstat ---

    #[test]
    fn parse_numstat_multi_file_sums_correctly() {
        let text = "10\t2\tsrc/a.rs\n5\t0\tsrc/b.rs\n0\t7\tsrc/c.rs\n";
        let totals = parse_numstat(text);
        assert_eq!(totals.files_changed, 3);
        assert_eq!(totals.insertions, 15);
        assert_eq!(totals.deletions, 9);
    }

    #[test]
    fn parse_numstat_binary_dash_counts_as_zero_but_still_a_file() {
        let text = "10\t2\tsrc/a.rs\n-\t-\tassets/image.png\n";
        let totals = parse_numstat(text);
        assert_eq!(totals.files_changed, 2);
        assert_eq!(totals.insertions, 10);
        assert_eq!(totals.deletions, 2);
    }

    #[test]
    fn parse_numstat_empty_input_is_all_zero() {
        let totals = parse_numstat("");
        assert_eq!(totals, NumstatTotals::default());
    }

    #[test]
    fn parse_numstat_garbage_lines_are_skipped_fail_soft() {
        let text = "not a numstat line at all\n10\t2\tsrc/a.rs\n\tmissing-fields\nabc\tdef\tsrc/bad.rs\n5\t1\tsrc/b.rs\n";
        let totals = parse_numstat(text);
        // Only the two well-formed lines (10/2/a.rs and 5/1/b.rs) count;
        // the garbage lines never panic and are simply skipped.
        assert_eq!(totals.files_changed, 2);
        assert_eq!(totals.insertions, 15);
        assert_eq!(totals.deletions, 3);
    }

    #[test]
    fn parse_numstat_blank_lines_are_ignored() {
        let text = "10\t2\tsrc/a.rs\n\n\n5\t1\tsrc/b.rs\n";
        let totals = parse_numstat(text);
        assert_eq!(totals.files_changed, 2);
    }

    // --- has_rationale ---

    #[test]
    fn has_rationale_present_when_non_empty_body() {
        assert!(has_rationale("this changes X because Y was broken"));
    }

    #[test]
    fn has_rationale_absent_when_empty_or_whitespace_only() {
        assert!(!has_rationale(""));
        assert!(!has_rationale("   \n\t  "));
    }

    // --- has_task_link ---

    #[test]
    fn has_task_link_absent_for_bare_8hex_without_context() {
        // A bare 8-hex token is indistinguishable from a git short-SHA, so
        // it must NOT be treated as a task link without explicit context.
        assert!(!has_task_link("fix(condukt): close gap (c3dcbd6d)"));
    }

    #[test]
    fn has_task_link_present_for_backlog_prefixed_id() {
        assert!(has_task_link("addresses backlog: c3dcbd6d"));
        assert!(has_task_link("Backlog #1234abcd follow-up"));
    }

    #[test]
    fn has_task_link_absent_when_no_token() {
        assert!(!has_task_link("quick fix, no ticket"));
    }

    #[test]
    fn has_task_link_does_not_match_inside_a_longer_hex_run() {
        // A 40-char git SHA should NOT be mistaken for an 8-char backlog id.
        assert!(!has_task_link(
            "see commit 1234567890abcdef1234567890abcdef12345678"
        ));
    }

    #[test]
    fn has_task_link_absent_for_bare_git_short_sha() {
        // THE fix's RED->GREEN discriminator: a commit that only cites a
        // git short-SHA must not be mis-scored as task-linked.
        assert!(!has_task_link("revert of a1b2c3d4"));
        assert!(!has_task_link("cherry-picked from deadbeef"));
        assert!(!has_task_link("verifier-found in c3dcbd6d earlier"));
    }

    #[test]
    fn has_task_link_present_for_backlog_context_on_same_hex() {
        // The SAME hex that is rejected bare IS accepted with context.
        assert!(has_task_link("(backlog c3dcbd6d)"));
    }

    #[test]
    fn has_task_link_present_for_run_id() {
        assert!(has_task_link("shipped in run-20260713-012935"));
    }

    #[test]
    fn has_task_link_absent_for_run_word_prose() {
        assert!(!has_task_link("a run-time optimization"));
        assert!(!has_task_link("run-down of changes"));
    }
}
