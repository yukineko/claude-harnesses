//! Per-session ring buffer of recent events + per-pattern nudge bookkeeping,
//! persisted as one small JSON file so detection survives across the many
//! separate hook process invocations within a session.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::sig::Event;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Nudge {
    pub count: u32,
    pub last_seq: u64,
}

/// Persistent consecutive-repeat STREAK — the escalation counter for
/// `Kind::Repeat`. Deliberately NOT derived from any window content or
/// `Trip::key`: a bounded sliding-window key necessarily drifts with the
/// window (a wide edited body's shared token core never stabilizes; distinct
/// same-tool incidents pool into one never-resetting content key), which is
/// exactly what made a long/wide drifting stuck loop fail to escalate
/// (CA-stuckguard-01 re-review). Instead this counts Repeat trips landing on
/// CONSECUTIVE event seqs and resets on any gap.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct RepeatRun {
    pub count: u32,
    pub last_seq: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SessionState {
    pub seq: u64,
    #[serde(default)]
    pub events: Vec<Event>,
    #[serde(default)]
    pub nudges: HashMap<String, Nudge>,
    /// `#[serde(default)]` so session-state JSON persisted by older builds
    /// (before this field existed) still deserializes, defaulting to a
    /// zeroed run.
    #[serde(default)]
    pub repeat_run: RepeatRun,
}

fn safe(session: &str) -> String {
    session
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

pub fn path(state_dir: &Path, session: &str) -> PathBuf {
    state_dir
        .join("sessions")
        .join(format!("{}.json", safe(session)))
}

pub fn load(state_dir: &Path, session: &str) -> SessionState {
    harness_core::store::load_json(&path(state_dir, session))
}

pub fn save(state_dir: &Path, session: &str, st: &SessionState) {
    harness_core::store::save_json(&path(state_dir, session), st);
}

impl SessionState {
    /// Append an event, assigning the next seq, and prune to `window`.
    pub fn push(&mut self, mut e: Event, window: usize) -> u64 {
        self.seq += 1;
        e.seq = self.seq;
        self.events.push(e);
        let len = self.events.len();
        if len > window {
            self.events.drain(0..len - window);
        }
        self.seq
    }

    /// Record a nudge for `key`; return the resulting (1-based) nudge count.
    pub fn record_nudge(&mut self, key: &str, seq: u64) -> u32 {
        let n = self.nudges.entry(key.to_string()).or_default();
        n.count += 1;
        n.last_seq = seq;
        n.count
    }

    /// Advance the persistent consecutive-repeat streak for a `Kind::Repeat`
    /// trip landing on event `cur_seq`, returning the streak's new length.
    ///
    /// This — not any `Trip::key` nudge count — is what drives repeat
    /// escalation. `cur_seq == last_seq + 1` means this trip immediately
    /// follows the previous one (the same ongoing run), so the streak
    /// increments; ANY gap resets it to 1. A gap arises whenever an
    /// intervening event did not trip a repeat (it still bumped `seq` via
    /// `push`, so its seq is skipped here), i.e. a genuinely different action
    /// broke the run — so two temporally-separated incidents never pool into
    /// one escalation. Crucially it depends only on seq adjacency, never on
    /// the body/token width, so a wide drifting edit loop (whose window token
    /// core never stabilizes) escalates just like a narrow one. Callers drive
    /// this on every consecutive Repeat trip *independently of cooldown*, so a
    /// cooldown gap in message emission does not reset an ongoing run.
    pub fn record_repeat_run(&mut self, cur_seq: u64) -> u32 {
        if cur_seq == self.repeat_run.last_seq + 1 {
            self.repeat_run.count += 1;
        } else {
            self.repeat_run.count = 1;
        }
        self.repeat_run.last_seq = cur_seq;
        self.repeat_run.count
    }

    /// True if this pattern was nudged within `cooldown` events of `seq`.
    pub fn in_cooldown(&self, key: &str, seq: u64, cooldown: u64) -> bool {
        self.nudges
            .get(key)
            .map(|n| seq.saturating_sub(n.last_seq) < cooldown)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeat_run_increments_on_consecutive_seqs_and_resets_on_gap() {
        let mut st = SessionState::default();

        // First trip: last_seq default 0, seq 3 is not 0+1 → starts at 1.
        assert_eq!(st.record_repeat_run(3), 1);
        // Consecutive seqs keep climbing (this is the escalation counter).
        assert_eq!(st.record_repeat_run(4), 2);
        assert_eq!(st.record_repeat_run(5), 3);

        // A gap (an intervening non-repeat event bumped seq to 6 without a
        // trip, so the next trip lands on 7, not 6) resets the streak — a
        // temporally-separated incident must not pool into the prior run.
        assert_eq!(st.record_repeat_run(7), 1);
        assert_eq!(st.record_repeat_run(8), 2);
    }

    #[test]
    fn repeat_run_defaults_for_older_persisted_state() {
        // Session-state JSON written before `repeat_run` existed must still
        // deserialize (serde default), starting the streak from zero.
        let old = r#"{ "seq": 5, "events": [], "nudges": {} }"#;
        let st: SessionState = serde_json::from_str(old).expect("old-shape state must parse");
        assert_eq!(st.repeat_run.count, 0);
        assert_eq!(st.repeat_run.last_seq, 0);
    }
}
