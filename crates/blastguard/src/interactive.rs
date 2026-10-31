//! Is a human present to answer a PreToolUse `ask`?
//!
//! [`crate::model::Decision::Ask`] is only a safe answer when someone can
//! actually respond to the permission prompt. Claude Code runs this hook in
//! contexts where nobody can: `claude -p` headless runs, cron jobs, condukt
//! workers, SDK-driven sessions. Emitting `ask` there does not pause for a
//! human — it degrades into a block the agent cannot clear.
//!
//! So the gate is POSITIVE, never optimistic: ask ONLY when the environment is
//! affirmatively recognised as an interactive terminal session. A missing,
//! unrecognised or ambiguous signal resolves to "no human" and the `Ask` is
//! hardened to a `Deny` by [`crate::model::Decision::hardened`]. This is the
//! same rule the rest of the crate follows — an unprovable check that defaulted
//! to asking would reintroduce exactly the "unknown → optimistic" bias this
//! module exists to remove.
//!
//! # The signal, and the evidence for it
//!
//! `CLAUDE_CODE_ENTRYPOINT`, which Claude Code sets in the environment of every
//! process it spawns (hooks included). Measured on this host by dumping `env`
//! from a Bash tool call in each mode (Claude Code 2.1.212 / 2.1.215):
//!
//! | mode                                   | `CLAUDECODE` | `CLAUDE_CODE_ENTRYPOINT` |
//! |----------------------------------------|--------------|--------------------------|
//! | interactive terminal REPL              | `1`          | `cli`                    |
//! | headless `claude -p '…'`               | `1`          | `sdk-cli`                |
//!
//! `CLAUDECODE=1` is set in BOTH modes, so it alone proves only "inside Claude
//! Code", never "a human is watching". `CLAUDE_CODE_ENTRYPOINT` is what
//! actually differs, so it is what this gate reads.
//!
//! Signals deliberately NOT used, and why:
//!   * `isatty(stdin/stdout)` — the hook's stdin is the payload pipe and its
//!     stdout is the decision pipe, so neither is ever a tty in either mode.
//!   * `TERM` / `TERM_PROGRAM` — INHERITED by the headless run from the parent
//!     shell. Both were identical (`tmux-256color`) in the two measurements
//!     above, so they discriminate nothing.
//!   * `CLAUDE_CODE_CHILD_SESSION`, `AI_AGENT` — likewise present and equal in
//!     both measurements.
//!
//! # Subagents
//!
//! The env above is INHERITED by subagents unchanged, so a subagent of an
//! interactive session also reads as `cli`. Nobody can answer its `ask`.
//! [`payload_is_subagent`] detects it from the payload instead, and the caller
//! turns every `Ask` there into a `Deny`, including the repeat-deny downgrade.
//! `BLASTGUARD_ASK=always` does not override this.
//!
//! # Residual gaps (see the crate README / task report)
//!
//! `cli` is treated as the ONLY affirmatively-interactive value because it is
//! the only one measured here. Other entrypoints exist (IDE extensions, SDK
//! bindings) and are NOT claimed to be non-interactive — they are merely
//! unproven, and unproven resolves to deny. An operator who has verified one of
//! them can force asking on with `BLASTGUARD_ASK=always`.
//!
//! An interactive `cli` session can still be running under a permission mode
//! that skips permission prompts (`bypassPermissions`,
//! `--dangerously-skip-permissions`, or a `dontAsk` setting). That mode is NOT
//! visible in the hook's environment or in the PreToolUse stdin payload this
//! crate parses, so it is not detected.
//!
//! Which direction that failure runs decides whether `Ask` is usable at all: if
//! a skipped prompt auto-APPROVES, every `Ask` in this crate is a silent Allow
//! in that mode. Until 2026-07-30 this paragraph asserted the safe direction
//! with no measurement behind it — the kind of unbacked claim CLAUDE.md §2
//! forbids. Measured:
//!
//! ```text
//! Claude Code 2.1.197, blastguard 0.2.35 (installed), headless `claude -p`,
//! measured 2026-07-30. BLASTGUARD_ASK=always forces this crate to emit `ask`
//! regardless of entrypoint, so what is under test is the CLI's permission
//! layer, not our own hardening. The probe's only observable effect is creating
//! a marker file, so "did the tool call run" is answered by the file existing.
//!
//!   blastguard  permission mode                 marker created?
//!   ----------  ------------------------------  ---------------
//!   allow       bypassPermissions               YES   <- rig control
//!   deny        bypassPermissions               no
//!   ask         bypassPermissions               no    <- the measurement
//!   ask         (default)                       no
//!   allow       --dangerously-skip-permissions  YES   <- rig control
//!   deny        --dangerously-skip-permissions  no
//!   ask         --dangerously-skip-permissions  no    <- the measurement
//!   allow       bypassPermissions + ASK=always  YES   <- proves the env var
//!               on a NON-protected target                is not the blocker
//! ```
//!
//! So under both bypass flags a PreToolUse `ask` did NOT execute the tool call:
//! it degrades toward refusal, not toward allowing an unanalysed command.
//!
//! Two limits on that result, stated so the next reader does not over-read it:
//! it was taken in HEADLESS `claude -p`, not in an interactive TTY started in
//! bypass mode; and the transcript did not echo this crate's reason string, so
//! "blastguard was the blocker" is attributed from the control rows (same env,
//! non-protected target, marker appears) rather than read off a message.

/// Value of the operator override env var.
const ASK_OVERRIDE_VAR: &str = "BLASTGUARD_ASK";

/// The one `CLAUDE_CODE_ENTRYPOINT` value measured to be an interactive,
/// human-attended terminal session. See the module docs for the measurement.
const INTERACTIVE_ENTRYPOINT: &str = "cli";

/// Whether this process may emit a PreToolUse `ask`.
///
/// Resolution order:
///   1. `BLASTGUARD_ASK=never`  → never ask (force the safe behaviour).
///   2. `BLASTGUARD_ASK=always` → always ask (operator has verified their env).
///   3. `BLASTGUARD_ASK=auto`   → environment detection (below).
///   4. unset                   → `auto`, the DEFAULT.
///   5. any other value         → treated as `never`. An unrecognised setting is
///      an unknown, and unknown resolves to the safe side, not to asking.
pub fn ask_available() -> bool {
    resolve(
        std::env::var(ASK_OVERRIDE_VAR).ok().as_deref(),
        std::env::var("CLAUDECODE").ok().as_deref(),
        std::env::var("CLAUDE_CODE_ENTRYPOINT").ok().as_deref(),
    )
}

/// Is this PreToolUse call being made from inside a subagent?
///
/// A subagent cannot answer a permission prompt: nobody is watching it, and a
/// background subagent that hit an `ask` on 2026-10-30 waited 10.6 h. The env
/// cannot tell the two apart — measured byte-identical between a main-thread
/// and a subagent Bash call, `CLAUDE_CODE_ENTRYPOINT=cli` included — so
/// [`ask_available`] alone says "a human can answer" inside a subagent. The
/// only measured discriminator is the payload: Claude Code adds `agent_id`
/// (and `agent_type`) to a subagent's PreToolUse payload, and the main
/// thread's carries neither.
///
/// Fail-closed (CLAUDE.md §3): the main thread is only recognised by the
/// ABSENCE of both keys. Any presence counts as a subagent, whatever the value
/// — a non-string, `null` or empty `agent_id` is "cannot tell", and "cannot
/// tell" must not read as "a human is here". A payload that is not JSON at all
/// cannot be inspected structurally, so a raw `"agent_id"` / `"agent_type"`
/// key anywhere in its bytes also counts as a subagent.
pub fn payload_is_subagent(raw: &str) -> bool {
    const KEYS: [&str; 2] = ["agent_id", "agent_type"];
    match serde_json::from_str::<serde_json::Value>(raw.trim()) {
        Ok(serde_json::Value::Object(map)) => KEYS.iter().any(|k| map.contains_key(*k)),
        // Valid JSON that is not an object carries no keys at all.
        Ok(_) => false,
        Err(_) => KEYS.iter().any(|k| raw.contains(&format!("\"{k}\""))),
    }
}

/// The whole decision as a pure function of the three env values.
///
/// Split out from [`ask_available`] so the tests can drive the REAL logic:
/// `ask_available` reads process-global env, and mutating that in tests would
/// race the other tests in this binary (`std::env::set_var` is `unsafe` from the
/// 2024 edition for exactly that reason). A test helper that re-implemented this
/// match instead would pass just as happily with the production version
/// inverted — it would be testing a copy, not the gate.
fn resolve(override_var: Option<&str>, claudecode: Option<&str>, entrypoint: Option<&str>) -> bool {
    match override_var {
        Some("never") => false,
        Some("always") => true,
        Some("auto") | None => {
            // Positive check: both markers must be present AND recognised.
            // `CLAUDECODE=1` is set in headless runs too, so it is necessary but
            // nowhere near sufficient; the entrypoint is what discriminates.
            claudecode == Some("1") && entrypoint == Some(INTERACTIVE_ENTRYPOINT)
        }
        // Includes the empty string and any typo'd value.
        Some(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_cli_session_may_ask() {
        // The measured interactive terminal REPL environment.
        assert!(resolve(None, Some("1"), Some("cli")));
        assert!(resolve(Some("auto"), Some("1"), Some("cli")));
    }

    #[test]
    fn headless_sdk_run_may_not_ask() {
        // The measured `claude -p` environment: CLAUDECODE=1 is present, so
        // only the entrypoint distinguishes it.
        assert!(!resolve(None, Some("1"), Some("sdk-cli")));
    }

    #[test]
    fn missing_or_unrecognised_signals_may_not_ask() {
        assert!(!resolve(None, None, None)); // bare shell / cron
        assert!(!resolve(None, Some("1"), None)); // entrypoint absent
        assert!(!resolve(None, None, Some("cli"))); // CLAUDECODE absent
        assert!(!resolve(None, Some("0"), Some("cli"))); // CLAUDECODE not "1"
        assert!(!resolve(None, Some("1"), Some("vscode"))); // unproven entrypoint
        assert!(!resolve(None, Some("1"), Some("sdk-py")));
        assert!(!resolve(None, Some("1"), Some(""))); // empty
    }

    #[test]
    fn override_never_wins_over_an_interactive_environment() {
        assert!(!resolve(Some("never"), Some("1"), Some("cli")));
    }

    #[test]
    fn override_always_wins_over_a_headless_environment() {
        assert!(resolve(Some("always"), Some("1"), Some("sdk-cli")));
        assert!(resolve(Some("always"), None, None));
    }

    #[test]
    fn unrecognised_override_value_resolves_to_never_not_to_asking() {
        // An unknown setting must not be optimistic — that is the whole bias
        // this module exists to remove.
        assert!(!resolve(Some("yes"), Some("1"), Some("cli")));
        assert!(!resolve(Some(""), Some("1"), Some("cli")));
        assert!(!resolve(Some("AUTO"), Some("1"), Some("cli"))); // case-sensitive
    }

    #[test]
    fn main_thread_payload_is_not_a_subagent() {
        assert!(!payload_is_subagent(
            r#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#
        ));
        assert!(!payload_is_subagent("[1, 2, 3]"));
        assert!(!payload_is_subagent("this is not json"));
    }

    #[test]
    fn any_agent_marker_is_a_subagent_whatever_its_value() {
        for v in [r#""ab06""#, r#""""#, "null", "7", "{}", "[]"] {
            let raw = format!(r#"{{"tool_name":"Bash","agent_id":{v}}}"#);
            assert!(payload_is_subagent(&raw), "agent_id={v}");
        }
        assert!(payload_is_subagent(
            r#"{"tool_name":"Bash","agent_type":"general-purpose"}"#
        ));
    }

    #[test]
    fn unparseable_payload_mentioning_agent_id_is_a_subagent() {
        assert!(payload_is_subagent(
            r#"{"agent_id":"ab06","tool_input":{"command":"rm -rf "#
        ));
    }

    #[test]
    fn ask_available_does_not_panic_in_the_real_environment() {
        // Whatever this test process's environment is, the gate must answer
        // without panicking (a panic here would make the verdict barrier block every call).
        let _ = ask_available();
    }

    #[test]
    fn ask_available_is_wired_to_resolve_with_the_real_env_vars() {
        // The tests above prove `resolve`. This one proves `ask_available`
        // actually CALLS it, with those three variables and in that order —
        // otherwise the suite could be green while the exported gate read the
        // wrong var (or nothing at all).
        assert_eq!(
            ask_available(),
            resolve(
                std::env::var(ASK_OVERRIDE_VAR).ok().as_deref(),
                std::env::var("CLAUDECODE").ok().as_deref(),
                std::env::var("CLAUDE_CODE_ENTRYPOINT").ok().as_deref(),
            )
        );
    }
}
