//! `sitecraft gate` — the Stop hook.
//!
//! Runs through `harness_core::gate::run::run_guarded` (gate name
//! `sitecraft`; a panic blocks). In a repo with `sitecraft.toml` it combines,
//! with [`Verdict::worst_of`]: trust of the config, `decide check`,
//! `check security`, and — only when the session wrote a file matching
//! `web_globs`, or when that cannot be determined — `serve start` +
//! `check pages` + `serve stop`. A repo without `sitecraft.toml` is allowed
//! and a `not-configured` line is appended to the state dir. Give-up is
//! bounded (`max_attempts`) and says NOTHING WAS VERIFIED when it allows.

use harness_core::hook::HookInput;

use crate::Verdict;

/// Evaluate the gate for one Stop payload (the testable core; no stdout, no
/// exit).
///
/// Contract: no `sitecraft.toml` at the project root → `Clean` (recorded as
/// not-configured); otherwise `Clean` only when every applicable check is
/// `Clean`, `Violation` when one found something, and `Undetermined` when
/// any could not conclude (untrusted/unreadable config included). An
/// unreadable or missing transcript does not make the result `Undetermined`
/// by itself: it means "web files may have changed", so pages are checked.
pub fn run(input: &HookInput) -> Verdict {
    Verdict::undetermined(format!(
        "sitecraft gate: not implemented yet (cwd {})",
        input.cwd_or_current().display()
    ))
}

/// The whole `sitecraft gate` subcommand: read the Stop payload from stdin,
/// evaluate under the panic barrier, print the Stop-hook decision, and
/// return the process exit code.
pub fn command() -> i32 {
    let v = Verdict::undetermined("sitecraft gate: not implemented yet");
    if let Some(decision) = v.stop_decision() {
        println!("{decision}");
    }
    crate::report("sitecraft gate", &v)
}
