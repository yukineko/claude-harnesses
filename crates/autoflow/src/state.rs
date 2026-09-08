use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// No action taken yet this session.
    #[default]
    Idle,
    /// Blocked once with a /record prompt; next Stop will check condukt.
    RecordRequested,
    /// In the condukt/backlog continuation loop. Continues (blocks) as long as
    /// work is *progressing* — no cumulative call-count ceiling. Only a
    /// stalled-progress streak (`decide_progress` → `EscalateStuck`) escalates,
    /// and even then it stays visible (blocks) rather than silently standing
    /// down. The only legitimate stop is an empty pending/open set (→ `Done`).
    Continuing,
    /// All condukt tasks are done; no more blocking this session.
    Done,
}

/// Progress-based stop decision for one continuation branch (the condukt
/// pending set, or the backlog open queue). See `decide_progress`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopDecision {
    /// Set is non-empty and either progressing (or the stuck streak has not yet
    /// reached the threshold) → block with a routine continuation message.
    Continue,
    /// Set is non-empty but the no-progress streak reached the threshold →
    /// block with a VISIBLE stuck/escalation message. (Never silently allows;
    /// the loop is not stopped — the next cycle still continues.)
    EscalateStuck,
    /// Set is empty (no remaining work ⇒ task complete) → allow (`Phase::Done`).
    /// This is the ONLY legitimate stop.
    DoneEmpty,
}

/// Progress-based stop judgement for a single continuation branch (the condukt
/// pending set, or the backlog open queue). Pure (no side effects) so it can be
/// unit-tested directly, and shared by both branches.
///
/// `current_set_size` is the current count of remaining work. `prev_set_size`
/// is the size observed at the previous Stop (`None` = first observation).
/// `streak` is the running count of consecutive no-progress observations.
///
/// Returns `(decision, next prev_set_size to persist, next streak to persist)`.
pub fn decide_progress(
    current_set_size: u32,
    prev_set_size: Option<u32>,
    streak: u32,
    stuck_threshold: u32,
) -> (StopDecision, Option<u32>, u32) {
    if current_set_size == 0 {
        return (StopDecision::DoneEmpty, Some(0), 0);
    }
    let progressed = match prev_set_size {
        None => true, // first observation counts as progress → continue
        Some(prev) => current_set_size < prev,
    };
    if progressed {
        (StopDecision::Continue, Some(current_set_size), 0)
    } else {
        let new_streak = streak + 1;
        if new_streak >= stuck_threshold {
            // stuck: surface a visible escalation and reset the streak to 0 (so
            // it re-escalates only after another `stuck_threshold` no-progress
            // observations, not on every subsequent Stop).
            (StopDecision::EscalateStuck, Some(current_set_size), 0)
        } else {
            (StopDecision::Continue, Some(current_set_size), new_streak)
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SessionState {
    pub phase: Phase,
    /// Pending-set size observed at the previous condukt Stop (`None` until the
    /// first observation). Drives progress detection for the condukt branch.
    #[serde(default)]
    pub condukt_prev_pending: Option<u32>,
    /// Consecutive no-progress observations for the condukt pending set.
    #[serde(default)]
    pub condukt_no_progress_streak: u32,
    /// Whether the Tier 2 delegation-record advisory has already fired this
    /// session (dedup — fires at most once). `#[serde(default)]` keeps older
    /// on-disk state files (without this field) deserializable.
    #[serde(default)]
    pub delegation_audit_warned: bool,
}

fn state_path(state_dir: &Path, session_id: &str) -> PathBuf {
    // `session_id` originates from hook input; sanitise it so it stays a single
    // component under `state_dir` and cannot traverse out via `../`.
    state_dir.join(format!(
        "{}.json",
        harness_core::store::safe_session(session_id)
    ))
}

pub fn load(state_dir: &Path, session_id: &str) -> SessionState {
    harness_core::store::load_json(&state_path(state_dir, session_id))
}

pub fn save(state_dir: &Path, session_id: &str, s: &SessionState) {
    harness_core::store::save_json(&state_path(state_dir, session_id), s);
}

/// Path of the "resume /flow after /compact" marker for a session, keyed on the
/// (sanitised) session id under `state_dir`. Mirrors ctxrot's
/// `<state_dir>/<safe>.distilled` idiom: PreCompact drops it, the next
/// UserPromptSubmit consumes it. `safe_session` guarantees the id stays a single
/// component (no `../` traversal).
pub fn resume_marker_path(state_dir: &Path, session_id: &str) -> PathBuf {
    state_dir.join(format!(
        "{}.resume-flow",
        harness_core::store::safe_session(session_id)
    ))
}

/// Drop the resume-flow marker for this session (best-effort; a failed write just
/// means no auto-resume next turn — never breaks the compaction).
pub fn write_resume_marker(state_dir: &Path, session_id: &str) {
    let _ = std::fs::create_dir_all(state_dir);
    let _ = std::fs::write(resume_marker_path(state_dir, session_id), b"1");
}

/// Consume (delete) this session's resume-flow marker, returning `true` iff it
/// existed. Deleting on read makes re-injection fire exactly once per `/compact`
/// (idempotent): a non-existent marker returns `false` with no side effect.
pub fn consume_resume_marker(state_dir: &Path, session_id: &str) -> bool {
    // remove_file → Ok only when a file was actually removed; NotFound → Err →
    // false. This avoids an exists()+remove TOCTOU and is the single "consume".
    std::fs::remove_file(resume_marker_path(state_dir, session_id)).is_ok()
}

#[cfg(test)]
mod stop_contract_tests {
    //! Pins the six `decide_progress` invariants from
    //! `stopcontract-spec.md` (the shared canonical spec for the
    //! autoflow stop contract), plus a serde backward-compat test for the
    //! `SessionState` field replacement (`condukt_prompts`/`backlog_prompts` →
    //! progress-tracking fields).
    //!
    //! RED: `decide_progress` / `StopDecision` do not exist yet — this module
    //! will fail to COMPILE until the production worker adds them to this
    //! file (or an equivalent seam). That compile failure is the intended RED
    //! observation for this test-authoring pass.
    use super::*;

    // ---- invariant 1: empty set → DoneEmpty is the ONLY stop, regardless of
    // prev/streak ----

    #[test]
    fn empty_set_is_always_done_regardless_of_prev_and_streak() {
        // Vary prev_set_size and streak widely; current_set_size == 0 must
        // always win with DoneEmpty per the spec ("常に DoneEmpty").
        for prev in [None, Some(0), Some(1), Some(999)] {
            for streak in [0, 1, 2, 3, 100] {
                let (decision, next_prev, next_streak) = decide_progress(0, prev, streak, 3);
                assert_eq!(
                    decision,
                    StopDecision::DoneEmpty,
                    "prev={prev:?} streak={streak} must yield DoneEmpty"
                );
                assert_eq!(next_prev, Some(0), "DoneEmpty must persist prev=Some(0)");
                assert_eq!(next_streak, 0, "DoneEmpty must reset streak to 0");
            }
        }
    }

    // ---- invariant 2: non-empty + first observation (prev=None) → Continue,
    // streak resets to 0 ----

    #[test]
    fn first_observation_with_nonempty_set_continues_with_streak_zero() {
        let (decision, next_prev, next_streak) = decide_progress(5, None, 0, 3);
        assert_eq!(decision, StopDecision::Continue);
        assert_eq!(next_prev, Some(5));
        assert_eq!(next_streak, 0);

        // Even if an inherited streak value were somehow nonzero, first
        // observation (prev=None) is defined as progressed and must still
        // reset streak to 0.
        let (decision2, next_prev2, next_streak2) = decide_progress(9, None, 2, 3);
        assert_eq!(decision2, StopDecision::Continue);
        assert_eq!(next_prev2, Some(9));
        assert_eq!(next_streak2, 0);
    }

    // ---- invariant 3: non-empty + size decreased (progress) → Continue,
    // streak 0, with NO cumulative-count ceiling across many iterations ----

    #[test]
    fn decreasing_size_continues_forever_with_no_iteration_count_ceiling() {
        // Simulate 10 strictly-progressing steps (10, 9, 8, ..., 1). Every
        // single step must be Continue with streak reset to 0 — there must be
        // no hidden call-count/cumulative-prompt ceiling (the exact defect
        // being removed: the old `condukt_prompts <= 4` proxy).
        let mut prev: Option<u32> = None;
        let mut streak: u32 = 0;
        let stuck_threshold = 3;
        for current in (1..=10u32).rev() {
            let (decision, next_prev, next_streak) =
                decide_progress(current, prev, streak, stuck_threshold);
            assert_eq!(
                decision,
                StopDecision::Continue,
                "iteration at current={current} (prev={prev:?}) must Continue — \
                 progress must never hit a cumulative escalation ceiling"
            );
            assert_ne!(
                decision,
                StopDecision::EscalateStuck,
                "a progressing sequence must never EscalateStuck no matter how many iterations"
            );
            assert_eq!(next_streak, 0, "progress must reset streak to 0 every time");
            assert_eq!(next_prev, Some(current));
            prev = next_prev;
            streak = next_streak;
        }
        // After 10 progressing iterations (far beyond any legacy count proxy
        // like the old `<= 4`), we must still simply be at Continue/streak 0.
        assert_eq!(streak, 0);
    }

    // ---- invariant 4: non-empty + no-progress (unchanged or increased),
    // streak+1 below threshold → Continue, streak incremented ----

    #[test]
    fn no_progress_below_threshold_continues_with_incremented_streak() {
        // size unchanged: 5 -> 5, streak 0 -> 1, threshold 3 not yet reached.
        let (decision, next_prev, next_streak) = decide_progress(5, Some(5), 0, 3);
        assert_eq!(decision, StopDecision::Continue);
        assert_eq!(next_prev, Some(5));
        assert_eq!(next_streak, 1);

        // size increased: 7 -> 9 (current > prev) also counts as no-progress.
        let (decision2, next_prev2, next_streak2) = decide_progress(9, Some(7), 1, 3);
        assert_eq!(decision2, StopDecision::Continue);
        assert_eq!(next_prev2, Some(9));
        assert_eq!(next_streak2, 2);
    }

    // ---- invariant 5: non-empty + no-progress, streak+1 reaches threshold →
    // EscalateStuck, streak reset to 0 ----

    #[test]
    fn no_progress_reaching_threshold_escalates_and_resets_streak() {
        // streak=2, threshold=3: new_streak = 3 >= 3 -> EscalateStuck, reset.
        let (decision, next_prev, next_streak) = decide_progress(5, Some(5), 2, 3);
        assert_eq!(decision, StopDecision::EscalateStuck);
        assert_eq!(next_prev, Some(5));
        assert_eq!(next_streak, 0);

        // threshold=1 (smallest meaningful threshold): first no-progress
        // observation (streak 0 -> 1) must escalate immediately.
        let (decision2, _next_prev2, next_streak2) = decide_progress(4, Some(4), 0, 1);
        assert_eq!(decision2, StopDecision::EscalateStuck);
        assert_eq!(next_streak2, 0);
    }

    // ---- invariant 6: non-empty NEVER returns DoneEmpty (core of the
    // silent-Done prohibition) ----

    #[test]
    fn nonempty_set_never_returns_done_empty() {
        // Sweep a range of nonempty current sizes, prev values, streaks, and
        // thresholds — none of them may ever produce DoneEmpty. Silently
        // resolving a nonempty pending/open set to Done is exactly the
        // defect (§4 隠蔽禁止 / silent Done) this contract forbids.
        for current in 1..=6u32 {
            for prev in [None, Some(0), Some(1), Some(3), Some(10)] {
                for streak in 0..=4u32 {
                    for threshold in 1..=4u32 {
                        let (decision, _, _) = decide_progress(current, prev, streak, threshold);
                        assert_ne!(
                            decision,
                            StopDecision::DoneEmpty,
                            "current={current} prev={prev:?} streak={streak} threshold={threshold} \
                             must never resolve to DoneEmpty while non-empty"
                        );
                    }
                }
            }
        }
    }

    // ---- serde backward compat: old on-disk SessionState JSON (old fields
    // condukt_prompts/backlog_prompts present, new progress fields absent)
    // must still deserialize, defaulting the new fields ----

    #[test]
    fn old_session_state_json_deserializes_with_defaulted_progress_fields() {
        // This is the literal shape SessionState used to serialize as, before
        // the condukt_prompts/backlog_prompts -> progress-field replacement.
        // It intentionally does NOT contain condukt_prev_pending /
        // condukt_no_progress_streak, and DOES contain three fields this struct
        // no longer has — condukt_prompts/backlog_prompts (the pre-migration
        // counters) and backlog_prev_open/backlog_no_progress_streak (retired
        // 2026-08-20 with the queue arm itself). serde must ignore all of them as
        // unknown extras rather than erroring, which is what lets a field be
        // removed without stranding every state file already on disk.
        let old_json = r#"{
            "phase": "continuing",
            "condukt_prompts": 3,
            "backlog_prompts": 1,
            "backlog_prev_open": 4,
            "backlog_no_progress_streak": 2,
            "delegation_audit_warned": false
        }"#;

        let s: SessionState = serde_json::from_str(old_json).expect(
            "old on-disk SessionState JSON (pre progress-field migration) must still deserialize",
        );

        assert_eq!(s.phase, Phase::Continuing);
        assert!(!s.delegation_audit_warned);
        // New progress-tracking fields must all default when absent from the
        // old on-disk shape.
        assert_eq!(s.condukt_prev_pending, None);
        assert_eq!(s.condukt_no_progress_streak, 0);
    }
}
