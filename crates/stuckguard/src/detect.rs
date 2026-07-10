//! Detectors over the per-session event window. Two signals, both computed
//! purely from tool inputs (no fragile result parsing required):
//!
//! - **repeat**: the same normalized action N times in the window.
//! - **oscillation**: edit thrash — the same file edited back and forth so a
//!   change is repeatedly undone and redone.

use crate::config::Config;
use crate::sig::Event;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Repeat,
    Oscillation,
}

pub struct Trip {
    /// Stable key for cooldown / escalation bookkeeping.
    pub key: String,
    pub kind: Kind,
    /// How many times the pattern occurred (repeat count / reversal count).
    pub count: usize,
    /// True when the repeated action also kept failing.
    pub all_errored: bool,
    /// Human detail for the message (command/file).
    pub detail: String,
    /// Normalized error signature shared by the repeated/failing events, when
    /// available (`Kind::Repeat` with `all_errored`, and every one of those
    /// events agrees on the same digest). Lets the lesson written on
    /// escalation be keyed on the *error class* rather than only on the
    /// free-text detail, so a recurring error class can retrieve its past
    /// lesson even if the surrounding command text drifts slightly.
    pub error_digest: Option<String>,
}

/// Inspect the window (whose last element is the just-recorded event) and return
/// the strongest stuck pattern, if any. Oscillation outranks plain repeat.
pub fn detect(window: &[Event], cfg: &Config) -> Option<Trip> {
    let cur = window.last()?;

    if let Some(t) = oscillation(window, cur, cfg) {
        return Some(t);
    }
    repeat(window, cur, cfg)
}

/// Jaccard similarity of two token sets: `|A ∩ B| / |A ∪ B|`, in `[0, 1]`.
/// Two empty sets are defined as fully similar (`1.0`) — mirrors "both sigs
/// are the same trivial/empty body".
fn jaccard(a: &std::collections::BTreeSet<String>, b: &std::collections::BTreeSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let intersection = a.intersection(b).count();
    let union = a.union(b).count();
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

/// True when `e` counts as a repeat of `cur`: either byte-identical `sig`, or
/// (when `cfg.similarity_threshold < 1.0`) same tool with token-bag Jaccard
/// overlap at or above the configured threshold. At the default
/// `similarity_threshold == 1.0` this reduces to exact-match only — the
/// pre-existing behavior — because two non-identical sigs can't reach a
/// full 1.0 Jaccard score unless their token bags are literally equal (in
/// which case `sig` would already match, since `sig` is a hash of the same
/// normalized body `tokens` is derived from).
fn is_repeat_of(e: &Event, cur: &Event, cfg: &Config) -> bool {
    if e.sig == cur.sig {
        return true;
    }
    if cfg.similarity_threshold >= 1.0 {
        return false;
    }
    e.tool == cur.tool && jaccard(&e.tokens, &cur.tokens) >= cfg.similarity_threshold
}

fn repeat(window: &[Event], cur: &Event, cfg: &Config) -> Option<Trip> {
    let same: Vec<&Event> = window
        .iter()
        .filter(|e| is_repeat_of(e, cur, cfg))
        .collect();
    if same.len() < cfg.repeat_threshold {
        return None;
    }
    let all_errored = same.iter().all(|e| e.error);
    // Only surface an error_digest when every occurrence in the run agrees on
    // the same normalized error signature — a mix of digests (or any missing
    // one) means the events aren't clearly the same error class, so leave it
    // None rather than pick an arbitrary one.
    let error_digest = if all_errored {
        let mut digests = same.iter().map(|e| e.failed_test_digest.as_deref());
        let first = digests.next().flatten();
        if first.is_some() && digests.all(|d| d == first) {
            first.map(str::to_string)
        } else {
            None
        }
    } else {
        None
    };
    // Stable cluster key: the OLDEST matched event's sig, not `cur.sig`.
    // `window.iter()` is chronological, so `same[0]` (the first match found
    // by the filter above) is the earliest event in the cluster. For
    // exact-repeat mode every member of `same` shares the identical `sig`
    // anyway, so this is a no-op there. For near-repeat mode
    // (`cfg.similarity_threshold < 1.0`) `cur.sig` is (by construction)
    // different on every call, so keying on it made `record_nudge` reset its
    // per-key count back to 1 every single time — `escalate_after` was never
    // reachable (finding 5). Keying on the cluster's oldest member instead
    // gives a representative that stays stable across successive calls (as
    // long as it remains inside the window and keeps matching the current
    // event), so the nudge count actually accumulates.
    let key_sig = same
        .first()
        .map(|e| e.sig.as_str())
        .unwrap_or(cur.sig.as_str());
    Some(Trip {
        key: format!("repeat:{key_sig}"),
        kind: Kind::Repeat,
        count: same.len(),
        all_errored,
        detail: format!("{} を {} 回", cur.tool, same.len()),
        error_digest,
    })
}

/// Count reversals on the current file: an edit that swaps a previous edit's
/// before/after (X→Y followed later by Y→X). Two such reversals = a full
/// oscillation cycle.
fn oscillation(window: &[Event], cur: &Event, cfg: &Config) -> Option<Trip> {
    let file = cur.file.as_ref()?;
    let edits: Vec<&Event> = window
        .iter()
        .filter(|e| e.file.as_ref() == Some(file) && e.old_h.is_some() && e.new_h.is_some())
        .collect();
    if edits.len() < 2 {
        return None;
    }
    let mut reversals = 0usize;
    for (i, later) in edits.iter().enumerate() {
        for earlier in &edits[..i] {
            if later.old_h == earlier.new_h && later.new_h == earlier.old_h {
                reversals += 1;
                break; // count each later edit as at most one reversal
            }
        }
    }
    if reversals < cfg.oscillation_threshold {
        return None;
    }
    Some(Trip {
        key: format!("osc:{file}"),
        kind: Kind::Oscillation,
        count: reversals,
        all_errored: false,
        detail: file.clone(),
        error_digest: None,
    })
}

/// The early, soft "progress may be stalling" advisory: a deterministic
/// `progress_score` in `[0, 1]` (higher = more likely stalling) computed from
/// three signals over the recent window, distinct from and always emitted
/// (when it fires) BELOW the severity of the hard repeat/oscillation
/// escalation above. No embeddings/RAG — reuses the same `sig`/`tokens`/
/// `failed_test_digest` fields the hard detectors already compute.
pub struct ProgressAdvisory {
    /// Combined score in `[0, 1]`; higher = more likely stalling.
    pub score: f64,
    /// `1 - (distinct sigs / window len)`; high = low action diversity.
    pub diversity_signal: f64,
    /// Fraction of the window sharing the current event's `sig`; high = the
    /// same state persists.
    pub stability_signal: f64,
    /// Fraction of *errored* events in the window that share the current
    /// event's `failed_test_digest`; high = the same error keeps recurring.
    pub error_recurrence_signal: f64,
}

/// Compute the 3-signal `progress_score` over `window` (whose last element is
/// the just-recorded event), or `None` if the window is shorter than
/// `cfg.progress_min_window` (too few samples to judge diversity/stability).
///
/// Signals (each in `[0, 1]`, higher = more "stalled"):
/// - **action_diversity**: `1 - distinct_sigs / len` — low distinct-action
///   ratio means the agent keeps doing the same handful of things.
/// - **state-hash stability**: fraction of the window whose `sig` equals the
///   current event's `sig` — high persistence of the identical action/state.
/// - **error recurrence**: among errored events in the window, the fraction
///   that share the current event's `failed_test_digest` (0 when the current
///   event isn't an error, or there's no digest to compare).
///
/// `progress_score` is the unweighted mean of the three signals actually
/// available and does not attempt to replace or preempt `detect()` — callers
/// gate this behind `cfg.progress_advisory_enabled` and only surface it when
/// the hard detector did NOT already trip, so it's strictly additive.
pub fn progress_score(window: &[Event], cfg: &Config) -> Option<ProgressAdvisory> {
    let cur = window.last()?;
    if window.len() < cfg.progress_min_window {
        return None;
    }

    let len = window.len() as f64;

    // (a) action_diversity: distinct sig ratio over the window.
    let distinct: std::collections::HashSet<&str> = window.iter().map(|e| e.sig.as_str()).collect();
    let diversity_signal = 1.0 - (distinct.len() as f64 / len);

    // (b) state-hash stability: how much of the window shares the CURRENT sig.
    let same_sig = window.iter().filter(|e| e.sig == cur.sig).count();
    let stability_signal = same_sig as f64 / len;

    // (c) error recurrence: among errored events, how many share the
    // current event's failed_test_digest. 0.0 when the current event has no
    // digest (not an error, or an error we couldn't fingerprint).
    let error_recurrence_signal = match &cur.failed_test_digest {
        Some(digest) => {
            let errored: Vec<&Event> = window.iter().filter(|e| e.error).collect();
            if errored.is_empty() {
                0.0
            } else {
                let matching = errored
                    .iter()
                    .filter(|e| e.failed_test_digest.as_deref() == Some(digest.as_str()))
                    .count();
                matching as f64 / errored.len() as f64
            }
        }
        None => 0.0,
    };

    let score = (diversity_signal + stability_signal + error_recurrence_signal) / 3.0;

    Some(ProgressAdvisory {
        score,
        diversity_signal,
        stability_signal,
        error_recurrence_signal,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sig::build;
    use serde_json::json;

    fn cfg() -> Config {
        Config::default()
    }

    fn ev(seq: u64, tool: &str, input: serde_json::Value) -> Event {
        let mut e = build(tool, Some(&input), None, true).unwrap();
        e.seq = seq;
        e
    }

    #[test]
    fn repeat_trips_at_threshold() {
        let cmd = json!({"command": "cargo test"});
        let w: Vec<Event> = (0..3).map(|i| ev(i, "Bash", cmd.clone())).collect();
        let t = detect(&w, &cfg()).expect("should trip");
        assert_eq!(t.kind, Kind::Repeat);
        assert_eq!(t.count, 3);
    }

    #[test]
    fn two_repeats_do_not_trip() {
        let cmd = json!({"command": "cargo test"});
        let w: Vec<Event> = (0..2).map(|i| ev(i, "Bash", cmd.clone())).collect();
        assert!(detect(&w, &cfg()).is_none());
    }

    #[test]
    fn oscillation_trips_on_back_and_forth() {
        // A->B, B->A, A->B  => 2 reversals
        let w = vec![
            ev(
                0,
                "Edit",
                json!({"file_path":"f.rs","old_string":"A","new_string":"B"}),
            ),
            ev(
                1,
                "Edit",
                json!({"file_path":"f.rs","old_string":"B","new_string":"A"}),
            ),
            ev(
                2,
                "Edit",
                json!({"file_path":"f.rs","old_string":"A","new_string":"B"}),
            ),
        ];
        let t = detect(&w, &cfg()).expect("should trip");
        assert_eq!(t.kind, Kind::Oscillation);
    }

    #[test]
    fn near_repeat_detected_when_threshold_set() {
        // Same tool, high token overlap (only one differing token: the
        // trailing test-name filter), but NOT byte-identical -> different
        // exact `sig`. With a similarity_threshold below the actual
        // overlap, these should count as a repeat.
        let mut c = cfg();
        // Pairwise Jaccard for these three commands is 4/6 ≈ 0.667 (4 shared
        // tokens: cargo/test/-p/stuckguard, 2 differing: the trailing name).
        c.similarity_threshold = 0.6;
        let w: Vec<Event> = vec![
            ev(
                0,
                "Bash",
                json!({"command": "cargo test -p stuckguard foo"}),
            ),
            ev(
                1,
                "Bash",
                json!({"command": "cargo test -p stuckguard bar"}),
            ),
            ev(
                2,
                "Bash",
                json!({"command": "cargo test -p stuckguard baz"}),
            ),
        ];
        // Sanity: these are NOT exact-sig matches (near-repeat only).
        assert_ne!(w[0].sig, w[1].sig);
        assert_ne!(w[1].sig, w[2].sig);

        let t = detect(&w, &c).expect("near-repeat should trip when threshold is set");
        assert_eq!(t.kind, Kind::Repeat);
        assert_eq!(t.count, 3);

        // Same window, unset (default) threshold -> exact-match only, so no
        // trip since no two of these three are byte-identical.
        assert!(
            detect(&w, &cfg()).is_none(),
            "default threshold must not detect near-repeats"
        );
    }

    #[test]
    fn near_repeat_not_detected_below_threshold() {
        // Same tool, but token overlap is low (mostly disjoint commands).
        // Even with a similarity_threshold set, overlap must stay below it.
        let mut c = cfg();
        c.similarity_threshold = 0.9;
        let w: Vec<Event> = vec![
            ev(
                0,
                "Bash",
                json!({"command": "cargo test -p stuckguard foo"}),
            ),
            ev(
                1,
                "Bash",
                json!({"command": "cargo test -p stuckguard bar"}),
            ),
            ev(
                2,
                "Bash",
                json!({"command": "cargo test -p stuckguard baz"}),
            ),
        ];
        // These three share 4 of 5 tokens each pairwise -> Jaccard = 4/6 ≈ 0.67,
        // below the 0.9 threshold, so no near-repeat should trip.
        assert!(detect(&w, &c).is_none());
    }

    #[test]
    fn similarity_threshold_one_matches_default_exact_behavior() {
        // Explicitly setting similarity_threshold = 1.0 must reproduce the
        // exact same detect() outcome as the (equivalent) default config,
        // for both an exact-repeat window and a near-but-not-exact window.
        let mut c = cfg();
        c.similarity_threshold = 1.0;
        assert_eq!(
            c.similarity_threshold,
            Config::default().similarity_threshold
        );

        let exact_cmd = json!({"command": "cargo test"});
        let exact_w: Vec<Event> = (0..3).map(|i| ev(i, "Bash", exact_cmd.clone())).collect();
        assert!(detect(&exact_w, &c).is_some());
        assert!(detect(&exact_w, &cfg()).is_some());

        let near_w: Vec<Event> = vec![
            ev(
                0,
                "Bash",
                json!({"command": "cargo test -p stuckguard foo"}),
            ),
            ev(
                1,
                "Bash",
                json!({"command": "cargo test -p stuckguard bar"}),
            ),
            ev(
                2,
                "Bash",
                json!({"command": "cargo test -p stuckguard baz"}),
            ),
        ];
        assert_eq!(
            detect(&near_w, &c).is_some(),
            detect(&near_w, &cfg()).is_some()
        );
        assert!(detect(&near_w, &c).is_none());
    }

    /// finding 5: `Trip::key` used to be `format!("repeat:{}", cur.sig)` — the
    /// signature of the JUST-recorded event. Near-repeat matches are (by
    /// definition, since `similarity_threshold < 1.0`) never byte-identical,
    /// so `cur.sig` is different on every single call. Because
    /// `record_nudge` keys its per-pattern counter off `Trip::key`, that made
    /// the counter reset to 1 on every call — `escalate_after` could never be
    /// reached for a near-repeat pattern. This drives a realistic sequence
    /// of near-repeat (never byte-identical) events through the same
    /// detect()+record_nudge() loop `watch()` uses and asserts the nudge
    /// count actually climbs to `escalate_after` (i.e. escalation actually
    /// fires) instead of staying pinned at 1 forever. Before the finding-5
    /// fix this test fails (RED): the count would be 1 after every trip and
    /// `escalated` would never become true.
    #[test]
    fn near_repeat_escalates_after_repeated_nudges() {
        let mut c = cfg();
        c.similarity_threshold = 0.6;
        c.repeat_threshold = 3;
        c.escalate_after = 2;
        c.cooldown_events = 0; // never suppress a repeat trip via cooldown

        let mut st = crate::state::SessionState::default();
        let cmds = [
            "cargo test -p stuckguard foo",
            "cargo test -p stuckguard bar",
            "cargo test -p stuckguard baz",
            "cargo test -p stuckguard qux",
            "cargo test -p stuckguard zap",
        ];

        let mut escalated = false;
        let mut last_count = 0u32;
        let mut last_key: Option<String> = None;
        for (i, cmd) in cmds.iter().enumerate() {
            let e = ev(i as u64, "Bash", json!({"command": cmd}));
            let seq = st.push(e, c.window);
            if let Some(t) = detect(&st.events, &c) {
                if let Some(prev_key) = &last_key {
                    // Once the cluster has formed, the key must stay STABLE
                    // across calls — this is exactly what the finding-5 fix
                    // provides (and what `cur.sig`-keying broke).
                    if t.count >= c.repeat_threshold && last_count > 0 {
                        assert_eq!(
                            &t.key, prev_key,
                            "near-repeat cluster key must stay stable across calls once formed"
                        );
                    }
                }
                last_key = Some(t.key.clone());
                if !st.in_cooldown(&t.key, seq, c.cooldown_events) {
                    last_count = st.record_nudge(&t.key, seq);
                    if last_count >= c.escalate_after {
                        escalated = true;
                    }
                }
            }
        }

        assert!(
            escalated,
            "near-repeat pattern must eventually escalate (nudge count reaching \
             escalate_after={}); last observed count={last_count} — finding 5 regression \
             would keep this pinned at 1 forever",
            c.escalate_after
        );
    }

    /// Re-review regression (2026-07-10, still open): the finding-5 fix only
    /// keeps `Trip::key` stable while its anchor event (`same.first()`)
    /// remains inside the bounded sliding window (`cfg.window`, default 12 —
    /// eviction in `SessionState::push`). Once a near-repeat sequence runs
    /// longer than the window — the primary "stuck loop" scenario stuckguard
    /// exists to catch — each push evicts the anchor, `same.first()` rolls
    /// forward to a new event every call, and the nudge count resets to 1
    /// again, reproducing the original finding-5 bug for the tail of any
    /// long-running loop. The max nudge count reachable within one window is
    /// `window - repeat_threshold + 1` (10 at shipped defaults); this test
    /// sets `escalate_after` one above that ceiling and drives the loop for
    /// 30 calls (well past the window) to prove escalation still never
    /// fires. See docs/review-redesign-implementation-items.md, re-review
    /// finding 3.
    #[test]
    #[ignore = "known bug: near-repeat escalation resets once the anchor event \
                leaves the sliding window -- see \
                docs/review-redesign-implementation-items.md re-review finding 3"]
    fn near_repeat_escalates_even_past_window_boundary() {
        let mut c = cfg();
        c.similarity_threshold = 0.6;
        c.repeat_threshold = 3;
        c.cooldown_events = 0;
        // One above the max nudge count reachable within a single window
        // (window - repeat_threshold + 1 = 12 - 3 + 1 = 10): escalation can
        // only fire here if the key survives window eviction.
        c.escalate_after = 11;

        let mut st = crate::state::SessionState::default();
        let mut escalated = false;
        let mut last_count = 0u32;
        // 30 calls: well past the default window (12) -- a realistic length
        // for a genuinely stuck, long-running loop.
        for i in 0..30u64 {
            let cmd = format!("cargo test -p stuckguard variant-{i}");
            let e = ev(i, "Bash", json!({"command": cmd}));
            let seq = st.push(e, c.window);
            if let Some(t) = detect(&st.events, &c) {
                if !st.in_cooldown(&t.key, seq, c.cooldown_events) {
                    last_count = st.record_nudge(&t.key, seq);
                    if last_count >= c.escalate_after {
                        escalated = true;
                    }
                }
            }
        }

        assert!(
            escalated,
            "a near-repeat pattern that persists for 30 calls (well past the \
             window={} boundary) must eventually escalate; last observed \
             count={last_count} -- the anchor-eviction regression keeps \
             resetting the nudge count before escalate_after={} is ever \
             reached",
            c.window, c.escalate_after
        );
    }

    #[test]
    fn distinct_edits_do_not_trip() {
        let w = vec![
            ev(
                0,
                "Edit",
                json!({"file_path":"f.rs","old_string":"A","new_string":"B"}),
            ),
            ev(
                1,
                "Edit",
                json!({"file_path":"f.rs","old_string":"B","new_string":"C"}),
            ),
            ev(
                2,
                "Edit",
                json!({"file_path":"f.rs","old_string":"C","new_string":"D"}),
            ),
        ];
        assert!(detect(&w, &cfg()).is_none());
    }

    fn ev_err(
        seq: u64,
        tool: &str,
        input: serde_json::Value,
        response: serde_json::Value,
    ) -> Event {
        let mut e = build(tool, Some(&input), Some(&response), true).unwrap();
        e.seq = seq;
        e
    }

    #[test]
    fn progress_score_none_when_window_below_min() {
        let cmd = json!({"command": "cargo test"});
        let w: Vec<Event> = (0..3).map(|i| ev(i, "Bash", cmd.clone())).collect();
        let mut c = cfg();
        c.progress_min_window = 6;
        assert!(
            progress_score(&w, &c).is_none(),
            "window shorter than progress_min_window must yield None"
        );
    }

    #[test]
    fn diverse_actions_yield_low_progress_score() {
        // Every action distinct (different file/content), no errors -> all 3
        // signals should be low, so the combined score stays low.
        let w: Vec<Event> = (0..8)
            .map(|i| {
                ev(
                    i,
                    "Edit",
                    json!({
                        "file_path": format!("f{i}.rs"),
                        "old_string": format!("A{i}"),
                        "new_string": format!("B{i}"),
                    }),
                )
            })
            .collect();
        let c = cfg();
        let advisory = progress_score(&w, &c).expect("window large enough to score");
        assert!(
            advisory.score < c.progress_score_threshold,
            "diverse actions must NOT cross the advisory threshold: score={}",
            advisory.score
        );
    }

    #[test]
    fn low_diversity_stable_state_recurring_error_yields_high_progress_score() {
        // Same command repeated (low diversity, high stability) and every
        // occurrence fails with the same normalized error class (high error
        // recurrence) -> combined score should cross the conservative
        // threshold.
        let cmd = json!({"command": "cargo test"});
        let resp = json!({
            "exit_code": 1,
            "stderr": "thread 'main' panicked at src/lib.rs:10:3:\nassertion failed"
        });
        let w: Vec<Event> = (0..8)
            .map(|i| ev_err(i, "Bash", cmd.clone(), resp.clone()))
            .collect();
        let c = cfg();
        let advisory = progress_score(&w, &c).expect("window large enough to score");
        assert!(
            advisory.score >= c.progress_score_threshold,
            "low-diversity + stable + recurring-error window should cross the threshold: score={}",
            advisory.score
        );
        assert!(advisory.diversity_signal > 0.5);
        assert!(advisory.stability_signal > 0.5);
        assert!(advisory.error_recurrence_signal > 0.5);
    }
}
