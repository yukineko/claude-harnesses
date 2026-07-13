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
/// Two empty sets are defined as NOT similar (`0.0`), not fully similar. A
/// byte-identical trivial/empty body is already caught upstream by
/// `is_repeat_of`'s `e.sig == cur.sig` short-circuit *before* `jaccard` is
/// ever called, so by the time both token bags reach here empty, `sig` is
/// already known to differ — i.e. these are two genuinely DIFFERENT actions
/// (e.g. a Read with an empty `file_path` vs. one with a whitespace-only
/// `file_path`) that merely both tokenize to nothing. Treating that as a
/// perfect match (CA-stuckguard-02) produced spurious near-repeat nudges for
/// non-normalizing tools like Read/Grep/Glob whenever their body tokenized
/// empty; `0.0` (undefined overlap ⇒ no evidence of similarity) keeps real
/// repeat detection for normal, non-empty bodies untouched.
fn jaccard(a: &std::collections::BTreeSet<String>, b: &std::collections::BTreeSet<String>) -> f64 {
    let union = a.union(b).count();
    if union == 0 {
        0.0
    } else {
        let intersection = a.intersection(b).count();
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
    // Cooldown / message-dedup key ONLY. Escalation is no longer driven by
    // this key's nudge count — a bounded sliding-window content key
    // necessarily drifts with the window (CA-stuckguard-01 re-review): a wide
    // edited body's shared token core never stabilizes, so a `core:`-style key
    // churns every call and never escalates; a `tool:` fallback stops churning
    // but then pools temporally-separated same-tool incidents into one
    // never-resetting counter. Escalation therefore comes from a persistent
    // consecutive-trip STREAK held in `SessionState` (see
    // `state::SessionState::record_repeat_run` and the `watch()` caller). This
    // key only needs to be stable enough for short-term cooldown suppression:
    //   - exact-repeat mode (`similarity_threshold >= 1.0`): every member of
    //     `same` shares the identical `sig`, so key on `cur.sig` (the historical
    //     `repeat:{sig}` behavior).
    //   - near-repeat mode: near-repeat sigs differ every call, so a sig key
    //     would defeat cooldown entirely — key on the tool, which groups the
    //     near-repeat family for cooldown purposes without needing to be
    //     window-invariant.
    let key_sig = if cfg.similarity_threshold >= 1.0 {
        format!("sig:{}", cur.sig)
    } else {
        format!("tool:{}", cur.tool)
    };
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

    /// CA-stuckguard-02 (p2 — jaccard empty-set degenerate near-repeat): the
    /// `jaccard` "both empty -> 1.0" special case treats two DIFFERENT
    /// signatures as a perfect near-repeat match whenever both tokenize to an
    /// empty bag (e.g. Read with an empty vs. whitespace-only `file_path`).
    /// The genuinely-identical-trivial-body case is already covered by
    /// `is_repeat_of`'s `e.sig == cur.sig` short-circuit *before* jaccard is
    /// even consulted, so by the time jaccard's empty/empty branch is
    /// reached, `sig` is already known to differ — i.e. these are two
    /// distinct actions that both happen to tokenize to nothing, not "the
    /// same trivial body" the comment claimed. Before the fix this test
    /// fails (RED): two different empty-token Read calls falsely count as a
    /// near-repeat.
    #[test]
    fn empty_token_bodies_with_different_sig_are_not_near_repeats() {
        let mut c = cfg();
        c.similarity_threshold = 0.5;

        let a = ev(0, "Read", json!({"file_path": ""}));
        let b = ev(1, "Read", json!({"file_path": "\t"}));
        assert_ne!(a.sig, b.sig, "sanity: bodies must differ");
        assert!(a.tokens.is_empty(), "sanity: a tokenizes to empty");
        assert!(b.tokens.is_empty(), "sanity: b tokenizes to empty");

        assert!(
            !is_repeat_of(&a, &b, &c),
            "two different empty-token Read calls must not be treated as a near-repeat"
        );

        // Real repeat detection for normal (non-empty) bodies must be untouched.
        let x = ev(2, "Bash", json!({"command": "cargo test foo"}));
        let y = ev(3, "Bash", json!({"command": "cargo test foo"}));
        assert!(
            is_repeat_of(&x, &y, &c),
            "identical non-empty bodies must still count as a repeat"
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

    /// Mirror the escalation-counter half of `watch()`: for `Kind::Repeat`
    /// the counter is the persistent consecutive-trip STREAK (advanced on
    /// every trip, independent of cooldown); for `Kind::Oscillation` it's the
    /// `record_nudge` count. Returns `None` when the trip is in cooldown (no
    /// message would be emitted this call). This keeps the escalation tests in
    /// lockstep with the real `watch()` dispatch.
    fn drive_escalation(
        st: &mut crate::state::SessionState,
        t: &Trip,
        seq: u64,
        cfg: &Config,
    ) -> Option<u32> {
        let repeat_streak = match t.kind {
            Kind::Repeat => Some(st.record_repeat_run(seq)),
            Kind::Oscillation => None,
        };
        if st.in_cooldown(&t.key, seq, cfg.cooldown_events) {
            return None;
        }
        let nudge = st.record_nudge(&t.key, seq);
        Some(repeat_streak.unwrap_or(nudge))
    }

    /// A run of near-repeat (never byte-identical) events must escalate once
    /// the consecutive-trip streak reaches `escalate_after`. Escalation is
    /// driven by the persistent streak (`record_repeat_run`), not by any
    /// `Trip::key` nudge count — the historical finding-5 bug (keying on the
    /// churning `cur.sig`) can't recur because the streak is independent of the
    /// key entirely.
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
        for (i, cmd) in cmds.iter().enumerate() {
            let e = ev(i as u64, "Bash", json!({"command": cmd}));
            let seq = st.push(e, c.window);
            if let Some(t) = detect(&st.events, &c) {
                if let Some(count) = drive_escalation(&mut st, &t, seq, &c) {
                    last_count = count;
                    if count >= c.escalate_after {
                        escalated = true;
                    }
                }
            }
        }

        assert!(
            escalated,
            "near-repeat pattern must eventually escalate (streak reaching \
             escalate_after={}); last observed count={last_count}",
            c.escalate_after
        );
    }

    /// Re-review regression (CA-stuckguard-01, FIXED via streak): a near-repeat
    /// stuck loop that runs FAR longer than the sliding window (`cfg.window`,
    /// default 12) must still escalate. Any window-content key drifts as events
    /// are evicted, so no key-derived nudge count can be trusted to climb across
    /// the window boundary; the persistent consecutive-trip streak
    /// (`record_repeat_run`) is window-invariant by construction. This sets
    /// `escalate_after` far above the single-window ceiling
    /// (`window - repeat_threshold + 1`) and drives 30 calls to prove the streak
    /// keeps climbing past eviction.
    #[test]
    fn near_repeat_escalates_even_past_window_boundary() {
        let mut c = cfg();
        c.similarity_threshold = 0.6;
        c.repeat_threshold = 3;
        c.cooldown_events = 0;
        // Far above the max count reachable from within a single window
        // (window - repeat_threshold + 1 = 12 - 3 + 1 = 10): escalation can
        // only fire here if the counter survives window eviction.
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
                if let Some(count) = drive_escalation(&mut st, &t, seq, &c) {
                    last_count = count;
                    if count >= c.escalate_after {
                        escalated = true;
                    }
                }
            }
        }

        assert!(
            escalated,
            "a near-repeat pattern that persists for 30 calls (well past the \
             window={} boundary) must eventually escalate; last observed \
             count={last_count} -- escalate_after={} is only reachable if the \
             streak survives window eviction",
            c.window, c.escalate_after
        );
    }

    /// CA-stuckguard-01 (p1, HIGH — escalation fail-open): a family of
    /// DRIFTING near-repeats where each action is highly similar to its
    /// immediate neighbor but drifts far enough over several steps that it is
    /// no longer similar to an action from many steps back — so the window has
    /// no single stable shared token core. Any content-derived key therefore
    /// churns and never escalates; the persistent streak does. Each event here
    /// is 4 sliding-window tokens (`tokN tokN+1 tokN+2 tokN+3`); adjacent steps
    /// share 3 of 4 tokens (jaccard 0.6), 2-apart share 2 of 4 (jaccard 0.333)
    /// — both above the 0.3 threshold, so `is_repeat_of` trips every step — but
    /// nothing is shared across the whole run.
    #[test]
    fn drifting_near_repeat_family_still_escalates() {
        let mut c = cfg();
        c.similarity_threshold = 0.3;
        c.repeat_threshold = 3;
        c.escalate_after = 5;
        c.cooldown_events = 0;

        let mut st = crate::state::SessionState::default();
        let mut escalated = false;
        let mut last_count = 0u32;
        for i in 0..20u64 {
            let cmd = format!("tok{i} tok{} tok{} tok{}", i + 1, i + 2, i + 3);
            let e = ev(i, "Bash", json!({"command": cmd}));
            let seq = st.push(e, c.window);
            if let Some(t) = detect(&st.events, &c) {
                if let Some(count) = drive_escalation(&mut st, &t, seq, &c) {
                    last_count = count;
                    if count >= c.escalate_after {
                        escalated = true;
                    }
                }
            }
        }

        assert!(
            escalated,
            "a drifting near-repeat family (each step similar to its neighbor \
             but not to distant steps) must eventually escalate; last observed \
             count={last_count} -- escalate_after={} unreachable if escalation \
             is keyed on drifting window content instead of the streak",
            c.escalate_after
        );
    }

    /// CA-stuckguard-01 re-review, Finding 1 (critical residual fail-open): a
    /// WIDE near-repeat body (token width >= `cfg.window`) that drifts one
    /// token per step. Its shared-token intersection across the window never
    /// EMPTIES — it just shifts — so a `core:`-style content key churns every
    /// call and never triggers the old tool-name fallback, reproducing the
    /// original never-escalate bug. The persistent streak (independent of body
    /// width) fixes it. Mirrors the verifier's provided RED case, adapted to
    /// drive escalation off the streak (the actual mechanism).
    #[test]
    fn wide_body_drift_exceeding_window_still_escalates() {
        let mut c = cfg();
        c.similarity_threshold = 0.3;
        c.repeat_threshold = 3;
        c.escalate_after = 5;
        c.cooldown_events = 0;

        let mut st = crate::state::SessionState::default();
        let mut escalated = false;
        let mut last_count = 0u32;
        const L: u64 = 15; // >= cfg.window (12): body wider than the window
        for i in 0..40u64 {
            let body: Vec<String> = (0..L).map(|k| format!("tok{}", i + k)).collect();
            let e = ev(i, "Bash", json!({"command": body.join(" ")}));
            let seq = st.push(e, c.window);
            if let Some(t) = detect(&st.events, &c) {
                if let Some(count) = drive_escalation(&mut st, &t, seq, &c) {
                    last_count = count;
                    if count >= c.escalate_after {
                        escalated = true;
                    }
                }
            }
        }

        assert!(
            escalated,
            "wide-body drift (token width >= window) must still escalate; last \
             observed count={last_count} -- the window intersection never empties \
             so a content key churns forever; only the width-independent streak \
             reaches escalate_after={}",
            c.escalate_after
        );
    }

    /// CA-stuckguard-01 re-review, Finding 2 (over-aggregation): two SHORT
    /// near-repeat runs separated by several unrelated (non-repeat) events must
    /// NOT pool into a single escalation. Each run alone stays below
    /// `escalate_after`; the intervening events create a seq gap that resets the
    /// streak, so the second incident starts a fresh count rather than
    /// continuing the first. A `tool:`-fallback content key (which never resets)
    /// WOULD pool them and wrongly escalate — this asserts it does not.
    #[test]
    fn temporally_separated_repeat_incidents_do_not_pool() {
        let mut c = cfg();
        c.similarity_threshold = 0.3;
        c.repeat_threshold = 3;
        // Each 4-event run reaches a streak of 2 (trips on events 3 and 4);
        // pooled that would be >= 3 and escalate. With the gap reset, neither
        // run alone reaches 3.
        c.escalate_after = 3;
        c.cooldown_events = 0;

        // Run 1 (4 near-repeats), then a GAP of distinct actions (no repeat
        // trips -> seq advances without extending the run -> streak resets),
        // then Run 2 (a second, temporally-separated near-repeat family).
        let mut cmds: Vec<String> = Vec::new();
        for _ in 0..4 {
            cmds.push("alpha one two three".to_string());
        }
        for k in 0..4 {
            cmds.push(format!("unrelated-{k} distinct-{k} solo-{k}"));
        }
        for _ in 0..4 {
            cmds.push("beta four five six".to_string());
        }

        let mut st = crate::state::SessionState::default();
        let mut escalated = false;
        let mut max_count = 0u32;
        for cmd in cmds {
            let e = ev(0, "Bash", json!({"command": cmd}));
            let seq = st.push(e, c.window);
            if let Some(t) = detect(&st.events, &c) {
                if let Some(count) = drive_escalation(&mut st, &t, seq, &c) {
                    max_count = max_count.max(count);
                    if count >= c.escalate_after {
                        escalated = true;
                    }
                }
            }
        }

        assert!(
            !escalated,
            "two short, temporally-separated near-repeat incidents must NOT pool \
             into one escalation; max streak observed={max_count} reached \
             escalate_after={} -- a never-resetting content key would wrongly \
             pool them",
            c.escalate_after
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
