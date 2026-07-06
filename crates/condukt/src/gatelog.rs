//! The non-interactive question shim: `condukt policy answer`.
//!
//! `policy decide` maps a decision's risk×reversibility×confidence to
//! `auto|escalate|block`. This module turns that verdict into an *answer* to a
//! concrete question so a caller (the condukt/scout/flow skills) can stop
//! relying on the model obeying prose to "skip the AskUserQuestion when
//! autonomous". When the verdict is [`Decision::Auto`] the question is
//! self-answered with its pre-marked recommended option — no human prompt — and
//! the choice is recorded to an append-only decision log for audit. On
//! `Escalate`/`Block` nothing is answered and the caller falls through to a
//! real `AskUserQuestion` (escalate) or refuses (block).
//!
//! The verdict→answer mapping is a pure function ([`answer_outcome`]) so the
//! auto/escalate/exit-code contract is unit-testable without spawning a
//! process, and the log I/O mirrors [`crate::checkpoint`]'s fail-soft
//! append-only journal (a logging failure never breaks a turn).

use crate::policy::Decision;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One self-answered gate decision — the append-only audit record written when
/// (and only when) a question is auto-answered.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GateDecision {
    /// The question that was asked.
    pub question: String,
    /// The options that were offered.
    pub options: Vec<String>,
    /// 0-based index of the recommended (and, on auto, chosen) option.
    pub recommend_index: usize,
    /// The option that was chosen (== `options[recommend_index]`).
    pub chosen: String,
    /// The policy verdict that authorised the self-answer (always "auto" here —
    /// escalate/block are never journaled because nothing was answered).
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

/// Append one decision to the log. Fail-soft: any IO/serialize error is
/// swallowed so an audit-log failure never breaks a turn (a single-line append
/// is atomic on POSIX for our line sizes). Mirrors
/// [`crate::checkpoint::append_journal`].
pub fn append_decision(dir: &Path, entry: &GateDecision) {
    use std::io::Write;
    let Ok(line) = serde_json::to_string(entry) else {
        return;
    };
    let _ = std::fs::create_dir_all(dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(decisions_path(dir))
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Load the decision log in file order. A missing file yields an empty vec and
/// corrupt lines are skipped — never panics. Mirrors
/// [`crate::checkpoint::load_journal`].
pub fn load_decisions(dir: &Path) -> Vec<GateDecision> {
    let Ok(text) = std::fs::read_to_string(decisions_path(dir)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| serde_json::from_str::<GateDecision>(l).ok())
        .collect()
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

/// Append one circuit verdict to the run's log. Fail-soft: any IO/serialize
/// error is swallowed so a journaling failure never changes the gate's exit
/// code. Mirrors [`append_decision`].
pub fn append_circuit(dir: &Path, run_id: &str, entry: &CircuitRecord) {
    use std::io::Write;
    let Ok(line) = serde_json::to_string(entry) else {
        return;
    };
    let _ = std::fs::create_dir_all(dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(circuit_log_path(dir, run_id))
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Load a run's circuit-verdict log in file order. Missing file → empty vec;
/// corrupt lines are skipped — never panics. Mirrors [`load_decisions`]. The
/// read side of the append-only journal: exercised by tests and ready for a
/// future `circuit stats` aggregator; `append_circuit` is the live write path.
#[allow(dead_code)]
pub fn load_circuit_records(dir: &Path, run_id: &str) -> Vec<CircuitRecord> {
    let Ok(text) = std::fs::read_to_string(circuit_log_path(dir, run_id)) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<CircuitRecord>(l).ok())
        .collect()
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

/// Append one gate-exec verdict to the run's log. Fail-soft: any IO/serialize
/// error is swallowed so a journaling failure never changes the gate's exit
/// code. Mirrors [`append_circuit`].
pub fn append_gate_exec(dir: &Path, run_id: &str, entry: &GateExecRecord) {
    use std::io::Write;
    let Ok(line) = serde_json::to_string(entry) else {
        return;
    };
    let _ = std::fs::create_dir_all(dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(gate_exec_log_path(dir, run_id))
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Load a run's gate-exec-verdict log in file order. Missing file → empty vec;
/// corrupt lines are skipped — never panics. Mirrors [`load_circuit_records`].
/// The read side of the append-only journal: exercised by tests and ready for a
/// future `gate stats` aggregator; `append_gate_exec` is the live write path.
#[allow(dead_code)]
pub fn load_gate_exec_records(dir: &Path, run_id: &str) -> Vec<GateExecRecord> {
    let Ok(text) = std::fs::read_to_string(gate_exec_log_path(dir, run_id)) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<GateExecRecord>(l).ok())
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
}
