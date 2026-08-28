//! The in-flight ledger and the pure decision taken on it.
//!
//! Everything here is a pure function of (ledger, request): no I/O, no clock,
//! no env. The cap is therefore testable without a filesystem, and the decision
//! cannot depend on anything a test can't reproduce.

use serde::{Deserialize, Serialize};

/// Which pool a tool call draws from.
///
/// **Two pools, not one.** A single shared pool deadlocks: a subagent holds its
/// slot for its whole lifetime, so with the cap's worth of subagents live,
/// every `Bash` call *those same subagents* make would be denied — the holders
/// can never progress and so can never release. Pools keyed by class remove the
/// cycle: a subagent's `Bash` waits only behind other shells, never behind the
/// subagent slots that are waiting on it.
///
/// The quantity that actually froze this machine is concurrent OS processes,
/// and that is exactly what [`SlotClass::Shell`] bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SlotClass {
    /// A shell process: the `Bash` tool.
    Shell,
    /// A subagent: the `Task` / `Agent` tool.
    Subagent,
}

impl SlotClass {
    /// The pool a tool name draws from, or `None` for a tool this gate does not
    /// meter.
    ///
    /// `None` is NOT a verdict about the call — it means "not ours", and the
    /// caller must let such a call through untouched. Claude Code names the
    /// subagent tool `Task` on the wire and `Agent` in the tool list; both are
    /// accepted so that a rename cannot silently un-meter subagents.
    #[must_use]
    pub fn of_tool(tool_name: &str) -> Option<Self> {
        match tool_name {
            "Bash" => Some(Self::Shell),
            "Task" | "Agent" => Some(Self::Subagent),
            _ => None,
        }
    }

    /// Human-facing name used in the deny reason and in `status`.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Shell => "shell (Bash)",
            Self::Subagent => "subagent (Task/Agent)",
        }
    }

    /// Short tag for `status` output.
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Subagent => "subagent",
        }
    }
}

/// One tool call currently in flight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slot {
    pub class: SlotClass,
    /// Content hash of `tool_name` + `tool_input`, used to match the release to
    /// its acquire. Not unique — two identical concurrent commands share a key,
    /// which is why release removes ONE matching entry, never all of them.
    pub key: String,
    /// Unix seconds when the slot was taken. Reported by `status`; deliberately
    /// NOT used to expire slots (see [`Inflight::release`]).
    pub at: u64,
}

/// The per-session ledger of in-flight calls.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inflight {
    #[serde(default)]
    pub slots: Vec<Slot>,
    /// Unix seconds of the last mutation. `status` prints it so an operator can
    /// tell "the hook is running and nothing is in flight" from "the hook never
    /// ran" — the two produce an identical empty ledger otherwise (CLAUDE.md 3).
    #[serde(default)]
    pub updated_at: u64,
}

/// What `acquire` decided. `#[must_use]` because dropping it on the floor is
/// exactly the fail-open this gate exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub enum Decision {
    Allow,
    Deny(String),
}

impl Inflight {
    /// How many slots of `class` are currently held.
    #[must_use]
    pub fn count(&self, class: SlotClass) -> usize {
        self.slots.iter().filter(|s| s.class == class).count()
    }

    /// Take a slot for `class`, or refuse.
    ///
    /// `>=` (not `==`) is load-bearing: if a ledger somehow holds more than
    /// `cap` entries — a cap lowered mid-session, a hand-edited store — this
    /// still refuses instead of admitting even more.
    pub fn acquire(&mut self, class: SlotClass, key: &str, at: u64, cap: usize) -> Decision {
        let live = self.count(class);
        if live >= cap {
            return Decision::Deny(deny_reason(class, live, cap));
        }
        self.slots.push(Slot {
            class,
            key: key.to_string(),
            at,
        });
        self.updated_at = at;
        Decision::Allow
    }

    /// Give a slot of `class` back. Returns whether a slot was actually freed.
    ///
    /// Matching is by key first, then "oldest of this class". The fallback is
    /// deliberate and it is about the COUNT, not identity: a PostToolUse event
    /// means exactly one call of that class finished, so exactly one decrement
    /// is correct even if the payload's `tool_input` did not round-trip
    /// byte-identically. Without the fallback a key mismatch would leak a slot
    /// for the rest of the turn, shrinking the usable width until the next turn
    /// boundary.
    ///
    /// There is deliberately NO age-based expiry: "this slot looks old, assume
    /// it died" resolves an unknown to the permissive side, and would hand
    /// anyone a way to exceed the cap by simply taking long enough. Leaked
    /// slots are cleared at turn boundaries instead (`reset`).
    pub fn release(&mut self, class: SlotClass, key: &str, at: u64) -> bool {
        let exact = self
            .slots
            .iter()
            .position(|s| s.class == class && s.key == key);
        let idx = match exact {
            Some(i) => Some(i),
            None => self
                .slots
                .iter()
                .enumerate()
                .filter(|(_, s)| s.class == class)
                .min_by_key(|(_, s)| s.at)
                .map(|(i, _)| i),
        };
        match idx {
            Some(i) => {
                self.slots.remove(i);
                self.updated_at = at;
                true
            }
            None => false,
        }
    }
}

/// The text the model reads when a call is refused.
///
/// It must say three things or it will be misread as a failure of the command
/// itself: the call did NOT run, why, and what to do instead. "Re-issue it
/// alone" matters — re-sending the same batch just burns another round.
fn deny_reason(class: SlotClass, live: usize, cap: usize) -> String {
    format!(
        "parallelguard: this session already has {live} {} call(s) in flight, which is the \
         concurrency cap ({cap}). This call did NOT run — nothing was executed and nothing \
         failed. Wait for one of the in-flight calls to finish, then re-issue THIS call on its \
         own rather than re-sending the whole batch. Send at most {cap} {} calls per message.",
        class.label(),
        class.label(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAP: usize = 3;

    #[test]
    fn bash_and_task_draw_from_different_pools() {
        assert_eq!(SlotClass::of_tool("Bash"), Some(SlotClass::Shell));
        assert_eq!(SlotClass::of_tool("Task"), Some(SlotClass::Subagent));
        assert_eq!(SlotClass::of_tool("Agent"), Some(SlotClass::Subagent));
    }

    #[test]
    fn an_unmetered_tool_is_not_a_verdict() {
        // Read/Edit/Grep are not metered: `None` must stay distinguishable from
        // "allowed" so the caller passes them through without deciding.
        for t in ["Read", "Edit", "Write", "Grep", "WebFetch", ""] {
            assert_eq!(SlotClass::of_tool(t), None, "{t} must not be metered");
        }
    }

    #[test]
    fn the_fourth_concurrent_shell_is_denied() {
        let mut f = Inflight::default();
        for i in 0..3 {
            assert_eq!(
                f.acquire(SlotClass::Shell, &format!("k{i}"), 100, CAP),
                Decision::Allow,
                "call {i} should fit under the cap"
            );
        }
        match f.acquire(SlotClass::Shell, "k3", 100, CAP) {
            Decision::Deny(reason) => {
                assert!(reason.contains("did NOT run"), "reason was: {reason}");
                assert!(reason.contains('3'), "reason must name the cap: {reason}");
            }
            Decision::Allow => panic!("the 4th concurrent shell was admitted; cap not enforced"),
        }
        assert_eq!(
            f.count(SlotClass::Shell),
            3,
            "a denied call must not be recorded"
        );
    }

    #[test]
    fn a_full_shell_pool_does_not_block_subagents() {
        // The deadlock this design exists to avoid: shells at cap must not stop
        // a Task, and a full subagent pool must not stop a Bash.
        let mut f = Inflight::default();
        for i in 0..3 {
            let _ = f.acquire(SlotClass::Shell, &format!("s{i}"), 1, CAP);
        }
        assert_eq!(
            f.acquire(SlotClass::Subagent, "t0", 1, CAP),
            Decision::Allow
        );
        let mut g = Inflight::default();
        for i in 0..3 {
            let _ = g.acquire(SlotClass::Subagent, &format!("t{i}"), 1, CAP);
        }
        assert_eq!(g.acquire(SlotClass::Shell, "s0", 1, CAP), Decision::Allow);
    }

    #[test]
    fn releasing_frees_exactly_one_slot() {
        let mut f = Inflight::default();
        let _ = f.acquire(SlotClass::Shell, "a", 1, CAP);
        let _ = f.acquire(SlotClass::Shell, "a", 2, CAP); // the same command twice
        assert_eq!(f.count(SlotClass::Shell), 2);
        assert!(f.release(SlotClass::Shell, "a", 3));
        assert_eq!(
            f.count(SlotClass::Shell),
            1,
            "release must free ONE slot, not every match"
        );
    }

    #[test]
    fn release_with_an_unmatched_key_still_decrements_the_count() {
        // A tool_input that did not round-trip must not leak a slot.
        let mut f = Inflight::default();
        let _ = f.acquire(SlotClass::Shell, "acquired-key", 1, CAP);
        assert!(f.release(SlotClass::Shell, "different-key", 2));
        assert_eq!(f.count(SlotClass::Shell), 0);
    }

    #[test]
    fn release_never_raids_the_other_pool() {
        let mut f = Inflight::default();
        let _ = f.acquire(SlotClass::Subagent, "t", 1, CAP);
        assert!(!f.release(SlotClass::Shell, "t", 2));
        assert_eq!(f.count(SlotClass::Subagent), 1);
    }

    #[test]
    fn an_over_full_ledger_still_refuses() {
        // A cap lowered mid-session (HARNESS_MAX_PARALLEL=1) leaves more slots
        // held than the cap allows. `>=` keeps that refusing rather than
        // admitting more.
        let mut f = Inflight::default();
        for i in 0..5 {
            f.slots.push(Slot {
                class: SlotClass::Shell,
                key: format!("k{i}"),
                at: 1,
            });
        }
        assert!(matches!(
            f.acquire(SlotClass::Shell, "k5", 2, 3),
            Decision::Deny(_)
        ));
    }

    #[test]
    fn a_slot_never_expires_by_age() {
        // Age-based expiry would be a permissive guess; a long-running call
        // still holds its slot no matter how far the clock has moved.
        let mut f = Inflight::default();
        for (i, key) in ["ancient", "old", "recent"].iter().enumerate() {
            f.slots.push(Slot {
                class: SlotClass::Shell,
                key: (*key).to_string(),
                at: i as u64,
            });
        }
        assert!(matches!(
            f.acquire(SlotClass::Shell, "new", 999_999_999, 3),
            Decision::Deny(_)
        ));
    }
}
