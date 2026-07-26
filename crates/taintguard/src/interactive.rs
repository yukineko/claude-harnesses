//! Is a human present to answer a PreToolUse `ask`?
//!
//! Identical positive-check policy to `blastguard::interactive` (reproduced
//! here rather than imported, so taintguard stays a self-contained plugin
//! crate with no cross-gate dependency): ask ONLY when the environment is
//! affirmatively recognised as an interactive terminal session
//! (`CLAUDECODE=1` AND `CLAUDE_CODE_ENTRYPOINT=cli`, the one measured
//! interactive shape — see `blastguard/src/interactive.rs` module docs for the
//! measurement and the residual gaps). A missing, unrecognised, or ambiguous
//! signal resolves to "no human", and the `gate` subcommand hardens its `ask`
//! to a `deny` in that case — never to an allow.

/// The one `CLAUDE_CODE_ENTRYPOINT` value measured to be an interactive,
/// human-attended terminal session.
const INTERACTIVE_ENTRYPOINT: &str = "cli";

/// Whether this process may emit a PreToolUse `ask` (as opposed to hardening
/// it to a `deny`). Pure function of the two env vars, split out from any
/// direct env reads so tests can drive it without racing `std::env::set_var`
/// across the test binary (mirrors `blastguard::interactive::resolve`).
pub fn resolve(claudecode: Option<&str>, entrypoint: Option<&str>) -> bool {
    claudecode == Some("1") && entrypoint == Some(INTERACTIVE_ENTRYPOINT)
}

/// `resolve` wired to the real process environment.
pub fn ask_available() -> bool {
    resolve(
        std::env::var("CLAUDECODE").ok().as_deref(),
        std::env::var("CLAUDE_CODE_ENTRYPOINT").ok().as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_cli_session_may_ask() {
        assert!(resolve(Some("1"), Some("cli")));
    }

    #[test]
    fn headless_sdk_run_may_not_ask() {
        assert!(!resolve(Some("1"), Some("sdk-cli")));
    }

    #[test]
    fn missing_or_unrecognised_signals_may_not_ask() {
        assert!(!resolve(None, None));
        assert!(!resolve(Some("1"), None));
        assert!(!resolve(None, Some("cli")));
        assert!(!resolve(Some("0"), Some("cli")));
        assert!(!resolve(Some("1"), Some("vscode")));
        assert!(!resolve(Some("1"), Some("")));
    }

    #[test]
    fn ask_available_does_not_panic_in_the_real_environment() {
        let _ = ask_available();
    }
}
