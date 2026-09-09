//! The non-interactive question shim: `condukt policy answer`.
//!
//! `policy decide` maps a decision's risk×reversibility×confidence to
//! `auto|escalate|block`. This module turns that verdict into an *answer* to a
//! concrete question so a caller (the condukt/scout/flow skills) can stop
//! relying on the model obeying prose to "skip the AskUserQuestion when
//! autonomous". When the verdict is [`Decision::Auto`] the question is
//! self-answered with its pre-marked recommended option — no human prompt. On
//! `Escalate`/`Block` nothing is answered and the caller falls through to a
//! real `AskUserQuestion` (escalate) or refuses (block).
//!
//! **All three verdicts are journaled**, not just the self-answered one. The log
//! records that the gate was *consulted* and what it decided; `policy` says
//! which verdict came back and, on escalate/block, `chosen` is empty because
//! nothing was picked. Recording only the self-answers (the original contract)
//! made "the gate escalated to a human" and "the gate was never consulted"
//! byte-for-byte identical in the audit trail — the log is the review surface
//! that stands in for the prompts autonomy removed, so an absent row read as
//! "no gate fired". A malformed invocation (an out-of-range `--recommend`, an
//! unparseable level) is still **not** journaled: it is a rejected input, not a
//! decision.
//!
//! The verdict→answer mapping is a pure function ([`answer_outcome`]) so the
//! auto/escalate/exit-code contract is unit-testable without spawning a
//! process. The log I/O mirrors [`crate::checkpoint`]'s append-only journal:
//! best-effort, in that a logging failure never changes a verdict or an exit
//! code, but **never silent** — a lost record is reported on stderr.

use crate::policy::Decision;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One gate consultation — the append-only audit record written every time a
/// question is resolved against a policy verdict, whether or not it was
/// self-answered.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GateDecision {
    /// The question that was asked.
    pub question: String,
    /// The options that were offered.
    pub options: Vec<String>,
    /// 0-based index of the recommended option. It records what was *offered* as
    /// the recommendation, so it stays meaningful on escalate/block, where the
    /// recommendation existed but was not taken.
    pub recommend_index: usize,
    /// The option that was chosen. On `auto` this is `options[recommend_index]`;
    /// on escalate/block it is the **empty string**, because nothing was picked.
    ///
    /// Empty rather than absent on purpose: the field has no `serde(default)`,
    /// and [`load_decisions`] drops lines that fail to deserialize, so omitting
    /// the key would make every escalate row vanish silently from
    /// `condukt policy answers` — a hole in the very trail this records.
    #[serde(default)]
    pub chosen: String,
    /// The policy verdict this consultation returned: `"auto"` (self-answered
    /// with `chosen`), `"escalate"` (handed to a human) or `"block"` (refused).
    pub policy: String,
    /// Unix seconds when the decision was recorded.
    pub created_at: i64,
}

/// The outcome of resolving a question against a policy verdict. Pure and
/// exhaustive so the exit-code contract can be pinned by unit tests without a
/// process spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerOutcome {
    /// `Auto`: self-answered with `chosen` (== `options[recommend_index]`).
    Answered {
        chosen: String,
        recommend_index: usize,
    },
    /// `Escalate`: the caller must fall through to a real `AskUserQuestion`.
    Escalate,
    /// `Block`: a hard stop — the caller must not proceed.
    Block,
    /// The recommend index was out of range (no such option). Never silently
    /// picks the wrong option; the caller reports invalid input.
    Invalid,
}

impl AnswerOutcome {
    /// Exit-code contract, identical to `policy decide`:
    /// `0` = answered (auto), `2` = escalate, `3` = block, `1` = invalid input.
    pub fn exit_code(&self) -> i32 {
        match self {
            AnswerOutcome::Answered { .. } => 0,
            AnswerOutcome::Escalate => 2,
            AnswerOutcome::Block => 3,
            AnswerOutcome::Invalid => 1,
        }
    }
}

/// Resolve a question against a policy verdict. Pure. Only [`Decision::Auto`]
/// self-answers, and only when `recommend_index` names a real option; an
/// out-of-range index is [`AnswerOutcome::Invalid`] rather than a panic or a
/// silently-wrong pick.
pub fn answer_outcome(
    decision: Decision,
    options: &[String],
    recommend_index: usize,
) -> AnswerOutcome {
    match decision {
        Decision::Auto => match options.get(recommend_index) {
            Some(chosen) => AnswerOutcome::Answered {
                chosen: chosen.clone(),
                recommend_index,
            },
            None => AnswerOutcome::Invalid,
        },
        Decision::Escalate => AnswerOutcome::Escalate,
        Decision::Block => AnswerOutcome::Block,
    }
}

/// Path of the append-only gate-decisions log (JSONL) inside `dir`.
pub fn decisions_path(dir: &Path) -> PathBuf {
    dir.join("gate-decisions.jsonl")
}

/// Append one gate consultation to the log — auto, escalate or block alike.
///
/// Best-effort but **never silent**: an IO or serialize failure does not change
/// the command's exit code (the verdict has already been decided and is still
/// delivered on stdout), but it is reported on stderr. A dropped record would
/// otherwise make "the gate escalated" indistinguishable from "the gate was
/// never consulted", which is the same erasure this log exists to prevent.
/// Mirrors [`crate::checkpoint::append_journal`].
pub fn append_decision(dir: &Path, entry: &GateDecision) {
    let line = match serde_json::to_string(entry) {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "warning: gate-decisions: could not serialize the {} record ({e}) — \
                 this consultation is LOST from the audit trail",
                entry.policy
            );
            return;
        }
    };
    harness_core::append::append_line_reporting(&decisions_path(dir), &line, "gate-decisions");
}

/// Read a JSONL journal, reporting anything that could not be turned into a
/// record instead of dropping it.
///
/// A **missing** file is genuinely "nothing recorded yet" and stays quiet. Any
/// other read error, and every unparseable line, is announced on stderr: the
/// caller still gets the records it could recover (these are review surfaces,
/// and a partial history beats none), but the reader is told the list it is
/// looking at is short. Silently returning the survivors would let a truncated
/// log read as a complete one.
///
/// Note the remaining gap, tracked as backlog `d343ecbc`: the return type is
/// still a bare `Vec`, so "read failed" and "log is empty" are the same value to
/// a caller that ignores stderr. The principled fix is
/// `harness_core::verdict::Determination`; this only stops the loss being silent.
fn load_journal<T: serde::de::DeserializeOwned>(path: &Path, sink: &str) -> Vec<T> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            eprintln!(
                "warning: {sink}: could not read {} ({e}) — reporting an EMPTY history, \
                 which is not the same as an empty log",
                path.display()
            );
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    let mut skipped = 0usize;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str::<T>(line) {
            Ok(v) => out.push(v),
            Err(_) => skipped += 1,
        }
    }
    if skipped != 0 {
        eprintln!(
            "warning: {sink}: skipped {skipped} unreadable line(s) in {} — \
             the history shown is incomplete",
            path.display()
        );
    }
    out
}

/// Load the decision log in file order. Missing file → empty vec; unreadable
/// lines are skipped **and reported on stderr** — never panics. Mirrors
/// [`crate::checkpoint::load_journal`].
pub fn load_decisions(dir: &Path) -> Vec<GateDecision> {
    load_journal(&decisions_path(dir), "gate-decisions")
}

// ── Circuit-breaker verdict journal ─────────────────────────────────────────
//
// `condukt circuit check` appends one record per invocation here so the
// stop-condition history is observable. Reuses the same fail-soft append-only
// JSONL pattern as the gate-decisions log above (a journaling failure must
// never change the command's exit code). Kept as plain scalar fields (verdict/
// reason as strings) so a log with future/unknown values still round-trips.

/// One recorded CIRCUIT-BREAKER verdict — the append-only record written on
/// every `condukt circuit check --run RID`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CircuitRecord {
    /// `"continue"` or `"trip"`.
    pub verdict: String,
    /// The reason slug (`CircuitReason::as_str`) when tripped, else `None`.
    pub reason: Option<String>,
    /// The gathered consecutive-failure streak.
    pub streak: u32,
    /// The streak cap the decision used (0 disables the streak axis).
    pub streak_cap: u32,
    /// Whether the budget signal was over its cap.
    pub budget_over_cap: bool,
    /// The gathered idle duration in seconds.
    pub idle_secs: i64,
    /// The idle TTL the decision used (0 disables the stall axis).
    pub idle_ttl_secs: i64,
    /// Unix seconds when the verdict was recorded.
    pub recorded_at: i64,
}

/// Path of the per-run circuit-verdict log (JSONL) inside `dir`. The run id is
/// sanitised so a crafted id cannot escape `dir`.
pub fn circuit_log_path(dir: &Path, run_id: &str) -> PathBuf {
    dir.join(format!(
        "{}.circuit-log.jsonl",
        harness_core::store::safe_session(run_id)
    ))
}

/// Append one circuit verdict to the run's log. Best-effort but **never
/// silent**: a journaling failure never changes the gate's exit code, but it is
/// reported on stderr rather than dropped. Mirrors [`append_decision`].
pub fn append_circuit(dir: &Path, run_id: &str, entry: &CircuitRecord) {
    let line = match serde_json::to_string(entry) {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "warning: circuit log: could not serialize the {} record ({e}) — \
                 this verdict is LOST from the run history",
                entry.verdict
            );
            return;
        }
    };
    harness_core::append::append_line_reporting(
        &circuit_log_path(dir, run_id),
        &line,
        "circuit log",
    );
}

/// Load a run's circuit-verdict log in file order. Missing file → empty vec;
/// unreadable lines are skipped **and reported on stderr** — never panics. Mirrors [`load_decisions`]. The
/// read side of the append-only journal: exercised by tests and ready for a
/// future `circuit stats` aggregator; `append_circuit` is the live write path.
#[allow(dead_code)]
pub fn load_circuit_records(dir: &Path, run_id: &str) -> Vec<CircuitRecord> {
    load_journal(&circuit_log_path(dir, run_id), "circuit log")
}

// ── Gate-exec verdict journal ───────────────────────────────────────────────
//
// `condukt gate check` appends one record per invocation here so the
// auto-exec-vs-escalate history of gated tasks is observable. Same fail-soft
// append-only JSONL pattern as the gate-decisions / circuit logs above (a
// journaling failure must never change the command's exit code). Kept as plain
// scalar fields so a log with future/unknown values still round-trips.

/// One recorded gate-exec verdict — the append-only record written on every
/// `condukt gate check --run RID --task TASKID`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GateExecRecord {
    /// `"auto_exec"` or `"escalate"`.
    pub verdict: String,
    /// The gated task the decision was made for.
    pub task: String,
    /// The graded risk slug (`"low"`/`"medium"`/`"high"`), or `None` when the
    /// signals could not be gathered (fail-soft escalate).
    pub risk: Option<String>,
    /// Whether the action was reversible, or `None` on a signal-gathering miss.
    pub reversible: Option<bool>,
    /// Whether the prevailing run policy was auto (autonomous mode).
    pub policy_is_auto: bool,
    /// Unix seconds when the verdict was recorded.
    pub recorded_at: i64,
}

/// Path of the per-run gate-exec-verdict log (JSONL) inside `dir`. The run id is
/// sanitised so a crafted id cannot escape `dir`.
pub fn gate_exec_log_path(dir: &Path, run_id: &str) -> PathBuf {
    dir.join(format!(
        "{}.gate-exec-log.jsonl",
        harness_core::store::safe_session(run_id)
    ))
}

/// Append one gate-exec verdict to the run's log. Best-effort but **never
/// silent**: a journaling failure never changes the gate's exit code, but it is
/// reported on stderr rather than dropped. Mirrors [`append_circuit`].
pub fn append_gate_exec(dir: &Path, run_id: &str, entry: &GateExecRecord) {
    let line = match serde_json::to_string(entry) {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "warning: gate-exec log: could not serialize a record ({e}) — \
                 this verdict is LOST from the run history"
            );
            return;
        }
    };
    harness_core::append::append_line_reporting(
        &gate_exec_log_path(dir, run_id),
        &line,
        "gate-exec log",
    );
}

/// Load a run's gate-exec-verdict log in file order. Missing file → empty vec;
/// unreadable lines are skipped **and reported on stderr** — never panics. Mirrors [`load_circuit_records`].
/// The read side of the append-only journal: exercised by tests and ready for a
/// future `gate stats` aggregator; `append_gate_exec` is the live write path.
#[allow(dead_code)]
pub fn load_gate_exec_records(dir: &Path, run_id: &str) -> Vec<GateExecRecord> {
    load_journal(&gate_exec_log_path(dir, run_id), "gate-exec log")
}

// ── Post-execution diff-risk outcome journal ────────────────────────────────
//
// `crate::diffrisk_record::record_post_execution_diff_risk` appends one record
// per invocation here. Same fail-soft append-only JSONL pattern as the three
// journals above.
//
// WHY THIS EXISTS (and why a bool was not enough). That hook is the only live
// consumer of the call graph, and it reported its result as `bool`. Four
// materially different endings all mapped to `false`:
//
//   * the task had no worktree, so nothing was ever inspected;
//   * the worktree yielded no diff, so nothing was ever inspected;
//   * the diff WAS classified and came back below High — a real clean result;
//   * the classification said High but the overwatch append then failed.
//
// From outside, an empty violation registry is therefore consistent with both
// "the fleet is clean" and "this hook never actually ran on anything", and
// nothing on disk could separate them. That is precisely the shape CLAUDE.md
// 1 and 3 forbid: silence must not be readable as "checked, fine". This
// journal makes the *reason* durable, so a zero count becomes an answerable
// question instead of an ambiguous one.

/// Why one post-execution diff-risk invocation ended the way it did. Ordered
/// from "never looked" to "looked and acted", so a reader can tell a blind spot
/// (`NoWorktree`/`NoDiff`) apart from a real observation (`ClassifiedNotHigh`)
/// apart from a finding (`Recorded`) apart from a broken measurement
/// (`Undetermined`/`RecordFailed`).
#[must_use = "an outcome that is dropped re-creates the fail-open this type exists to close: `ClassifiedNotHigh` is the ONLY value meaning \"inspected and clean\""]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiffRiskOutcome {
    /// Gate 1: the task carried no worktree path. NOT inspected — this is a
    /// blind spot, not a clean bill of health.
    NoWorktree,
    /// Gate 2: a worktree existed but produced no usable diff (absent on disk,
    /// git failure, or an empty diff). NOT inspected — also a blind spot.
    NoDiff,
    /// The diff was actually classified and came back below High. This is the
    /// only value that means "we looked and it was fine".
    ClassifiedNotHigh,
    /// The diff classified High and a violation was appended to overwatch.
    Recorded,
    /// The classifier could not determine a risk (e.g. the sensitive-glob list
    /// failed to compile). Recorded under its own violation signature.
    Undetermined,
    /// A High verdict was reached but the overwatch append failed, so the
    /// finding exists and is NOT in the registry. Deliberately distinct from
    /// `Recorded`: a lost finding must never read as a filed one.
    RecordFailed,
}

impl DiffRiskOutcome {
    /// Stable slug for display/aggregation.
    pub fn as_str(self) -> &'static str {
        match self {
            DiffRiskOutcome::NoWorktree => "no-worktree",
            DiffRiskOutcome::NoDiff => "no-diff",
            DiffRiskOutcome::ClassifiedNotHigh => "classified-not-high",
            DiffRiskOutcome::Recorded => "recorded",
            DiffRiskOutcome::Undetermined => "undetermined",
            DiffRiskOutcome::RecordFailed => "record-failed",
        }
    }

    /// Whether the diff was actually put through the classifier. `false` marks
    /// the blind spots — the population a zero-finding count says nothing about.
    pub fn inspected(self) -> bool {
        !matches!(self, DiffRiskOutcome::NoWorktree | DiffRiskOutcome::NoDiff)
    }
}

/// One recorded post-execution diff-risk invocation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiffRiskRecord {
    /// The outcome slug (`DiffRiskOutcome::as_str`).
    pub outcome: String,
    /// Whether the diff reached the classifier at all.
    pub inspected: bool,
    /// The run the task belongs to.
    pub run_id: String,
    /// The task inspected.
    pub task_id: String,
    /// How many changed symbols the call graph extracted from the diff, when it
    /// ran. `None` on the blind-spot paths — deliberately not `0`, which would
    /// claim "the call graph ran and found nothing".
    pub changed_symbols: Option<usize>,
    /// How many caller sites the call graph enumerated, when it ran. `None` on
    /// the blind-spot paths, for the same reason.
    pub caller_sites: Option<usize>,
    /// Unix seconds when the outcome was recorded.
    pub recorded_at: i64,
}

/// Path of the post-execution diff-risk outcome log (JSONL) inside `dir`.
pub fn diffrisk_outcomes_path(dir: &Path) -> PathBuf {
    dir.join("diffrisk-outcomes.jsonl")
}

/// Append one diff-risk outcome. Best-effort but **never silent**: a journaling
/// failure never changes the hook's (already fail-soft) behaviour, and it is
/// reported on stderr so a reader can tell this history has a hole in it.
/// Mirrors [`append_decision`].
pub fn append_diffrisk_outcome(dir: &Path, entry: &DiffRiskRecord) {
    let Ok(line) = serde_json::to_string(entry) else {
        return;
    };
    let _ = std::fs::create_dir_all(dir);
    harness_core::append::append_line_reporting(
        &diffrisk_outcomes_path(dir),
        &line,
        "diffrisk-outcomes",
    );
}

/// Load the diff-risk outcome log in file order. Missing file → empty vec;
/// corrupt lines are skipped — never panics. Mirrors [`load_decisions`].
pub fn load_diffrisk_outcomes(dir: &Path) -> Vec<DiffRiskRecord> {
    let Ok(text) = std::fs::read_to_string(diffrisk_outcomes_path(dir)) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<DiffRiskRecord>(l).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> Vec<String> {
        vec!["adopt".to_string(), "revise".to_string()]
    }

    #[test]
    fn auto_self_answers_the_recommended_option() {
        let o = answer_outcome(Decision::Auto, &opts(), 1);
        assert_eq!(
            o,
            AnswerOutcome::Answered {
                chosen: "revise".to_string(),
                recommend_index: 1,
            }
        );
        assert_eq!(o.exit_code(), 0);
    }

    #[test]
    fn escalate_defers_to_a_human() {
        let o = answer_outcome(Decision::Escalate, &opts(), 0);
        assert_eq!(o, AnswerOutcome::Escalate);
        assert_eq!(o.exit_code(), 2);
    }

    #[test]
    fn block_is_a_hard_stop() {
        let o = answer_outcome(Decision::Block, &opts(), 0);
        assert_eq!(o, AnswerOutcome::Block);
        assert_eq!(o.exit_code(), 3);
    }

    #[test]
    fn out_of_range_recommend_is_invalid_not_a_panic() {
        let o = answer_outcome(Decision::Auto, &opts(), 9);
        assert_eq!(o, AnswerOutcome::Invalid);
        assert_eq!(o.exit_code(), 1);
    }

    #[test]
    fn empty_options_on_auto_is_invalid() {
        let o = answer_outcome(Decision::Auto, &[], 0);
        assert_eq!(o, AnswerOutcome::Invalid);
    }

    #[test]
    fn append_then_load_round_trips_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let mk = |q: &str, at: i64| GateDecision {
            question: q.to_string(),
            options: opts(),
            recommend_index: 0,
            chosen: "adopt".to_string(),
            policy: "auto".to_string(),
            created_at: at,
        };
        append_decision(dir.path(), &mk("first?", 100));
        append_decision(dir.path(), &mk("second?", 200));
        let got = load_decisions(dir.path());
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].question, "first?");
        assert_eq!(got[1].question, "second?");
        assert_eq!(got[1].created_at, 200);
    }

    #[test]
    fn missing_log_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_decisions(dir.path()).is_empty());
    }

    #[test]
    fn circuit_append_then_load_round_trips_and_missing_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_circuit_records(dir.path(), "r1").is_empty());
        let rec = CircuitRecord {
            verdict: "trip".to_string(),
            reason: Some("failure_streak".to_string()),
            streak: 3,
            streak_cap: 3,
            budget_over_cap: false,
            idle_secs: 0,
            idle_ttl_secs: 1800,
            recorded_at: 42,
        };
        append_circuit(dir.path(), "r1", &rec);
        append_circuit(dir.path(), "r1", &rec);
        let got = load_circuit_records(dir.path(), "r1");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], rec);
        // a different run id has its own (still-empty) log
        assert!(load_circuit_records(dir.path(), "r2").is_empty());
    }

    #[test]
    fn gate_exec_append_then_load_round_trips_and_missing_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_gate_exec_records(dir.path(), "r1").is_empty());
        let rec = GateExecRecord {
            verdict: "auto_exec".to_string(),
            task: "t1".to_string(),
            risk: Some("low".to_string()),
            reversible: Some(true),
            policy_is_auto: true,
            recorded_at: 7,
        };
        append_gate_exec(dir.path(), "r1", &rec);
        append_gate_exec(dir.path(), "r1", &rec);
        let got = load_gate_exec_records(dir.path(), "r1");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], rec);
        // a different run id has its own (still-empty) log
        assert!(load_gate_exec_records(dir.path(), "r2").is_empty());
    }

    #[test]
    fn corrupt_lines_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let good = GateDecision {
            question: "ok?".to_string(),
            options: opts(),
            recommend_index: 0,
            chosen: "adopt".to_string(),
            policy: "auto".to_string(),
            created_at: 1,
        };
        append_decision(dir.path(), &good);
        // Corrupt the tail then append another good line.
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(decisions_path(dir.path()))
                .unwrap();
            writeln!(f, "{{not valid json").unwrap();
        }
        append_decision(dir.path(), &good);
        let got = load_decisions(dir.path());
        assert_eq!(got.len(), 2, "the corrupt middle line must be skipped");
    }

    /// Regression test for issue #15: concurrent appends via append_decision
    /// must never interleave records. With atomic single-write (body+'\n' in one
    /// buffer), every line parses as exactly one JSON object.
    #[test]
    fn concurrent_decision_appends_never_interleave() {
        let dir = tempfile::tempdir().unwrap();

        const THREADS: usize = 8;
        const PER_THREAD: usize = 100;

        std::thread::scope(|scope| {
            for t in 0..THREADS {
                let dir = dir.path().to_path_buf();
                scope.spawn(move || {
                    for i in 0..PER_THREAD {
                        let decision = GateDecision {
                            question: format!("q{}_{}", t, i),
                            options: vec!["a".to_string(), "b".to_string()],
                            recommend_index: 0,
                            chosen: "a".to_string(),
                            policy: "auto".to_string(),
                            created_at: (t * 1000 + i) as i64,
                        };
                        append_decision(&dir, &decision);
                    }
                });
            }
        });

        let text = std::fs::read_to_string(decisions_path(dir.path())).unwrap();
        let mut count = 0usize;
        for (n, l) in text.lines().enumerate() {
            if l.is_empty() {
                continue;
            }
            serde_json::from_str::<GateDecision>(l)
                .unwrap_or_else(|e| panic!("line {n} is not a single JSON object: {e} — {l:.120}"));
            count += 1;
        }
        assert_eq!(
            count,
            THREADS * PER_THREAD,
            "every appended decision must survive as exactly one parseable line"
        );
    }
}
