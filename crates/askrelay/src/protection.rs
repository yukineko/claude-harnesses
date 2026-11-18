//! askrelay's protection statement (backlog 3a8e3b73): what this gate
//! protects, from what, and on what grounds. Printed verbatim by
//! `askrelay --protects`, and read out of this file by
//! `scripts/check-gate-protection.py`, which requires every field non-empty.
//!
//! Keep each field one ordinary string literal (`\` line continuations are
//! fine): the checker parses the literal without building the crate and
//! reports anything else as undetermined. `grounds` must cite something that
//! can be checked — code or a measured incident — never a promise.

use harness_core::gate::Protection;

pub const PROTECTION: Protection = Protection {
    protects: "The user's adjudication of a subagent's permission Ask: a command \
        that a gate (today blastguard) had to deny inside a subagent, because \
        nobody there can answer a permission prompt, is not silently lost - \
        the main session cannot end its turn until it has re-run that exact \
        command under its own permission prompt and relayed the result to the \
        subagent, or recorded a reason for not doing so.",
    against: "A subagent's Ask that is denied and then forgotten: the subagent \
        reports it, the main agent does not act on it, and the work the human \
        was meant to approve or decline is dropped without anyone deciding. \
        The Stop hook (hooks/hooks.json, `askrelay stop`) blocks while any \
        queued Ask of the session is neither executed-and-relayed nor \
        dismissed with a non-blank reason, and also blocks when the queue \
        cannot be determined: an unreadable file, a line that does not parse, \
        an event naming an id with no queued line, an unparseable Stop \
        payload, a panic (run_guarded), or a missing platform binary \
        (bin/askrelay launcher). It never executes a stored command itself, \
        so it adds no way to run something a gate did not judge.",
    grounds: "User ruling 2026-11-14: the subagent's Ask is decided in the main \
        session, and the procedure is enforced by code because the main agent \
        forgets it. Code: crates/harness-core/src/relay.rs stop_outcome \
        (block on any unresolved entry, block on Err from load), \
        crates/askrelay/src/main.rs stop (run_guarded barrier; unparseable \
        payload blocks). Observed: a background subagent waited 10.6 h on an \
        unanswerable Ask (2026-10-30, crates/harness-core/src/hook.rs \
        payload_is_subagent), which is why such an Ask is hardened to a Deny \
        and relayed instead.",
};
