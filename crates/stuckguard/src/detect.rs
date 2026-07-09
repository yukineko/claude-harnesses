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

fn repeat(window: &[Event], cur: &Event, cfg: &Config) -> Option<Trip> {
    let same: Vec<&Event> = window.iter().filter(|e| e.sig == cur.sig).collect();
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
    Some(Trip {
        key: format!("repeat:{}", cur.sig),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sig::build;
    use serde_json::json;

    fn cfg() -> Config {
        Config::default()
    }

    fn ev(seq: u64, tool: &str, input: serde_json::Value) -> Event {
        let mut e = build(tool, Some(&input), None).unwrap();
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
}
