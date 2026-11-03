//! PreToolUse gate: deny edits to registered targets that are not ratified.
//!
//! SCAFFOLD: the gate answers `Undetermined` for every payload, which the CLI
//! turns into exit 2 (a PreToolUse block).

use harness_core::verdict::Verdict;

/// Judge one PreToolUse `payload` (the raw stdin JSON).
///
/// * `Clean` — allow (exit 0).
/// * `Violation` — deny (exit 2, reason on stderr).
/// * `Undetermined` — deny (exit 2). An empty or unparsable payload, an
///   unreadable store, or a failed target probe must land here.
///
/// SCAFFOLD: always `Undetermined`.
#[track_caller]
pub fn evaluate(payload: &str) -> Verdict {
    let _ = payload;
    Verdict::undetermined("fde: the PreToolUse gate is not implemented (scaffold)")
}
