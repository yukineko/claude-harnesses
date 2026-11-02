//! sitecraft — a site-building harness for Claude Code.
//!
//! The `/sitecraft` skill asks the user the design decisions a site needs
//! (driven by the user's purpose, not a fixed questionnaire), records them in
//! `sitecraft/decisions.toml`, and only then builds a working prototype. This
//! crate is the deterministic half: the decision record and its checks, the
//! dev-server manager, the broken-page and security checks, and the Stop gate.
//!
//! # Exit-code convention (every subcommand)
//!
//! | code | meaning |
//! |------|---------|
//! | `0`  | **clean** — the check ran to completion and found nothing (or the action succeeded) |
//! | `1`  | **violation** — the check ran and found something that must be fixed |
//! | `2`  | **undetermined** — the check could not run to a conclusion (IO error, parse failure, missing tool, empty scan set, untrusted config, …) |
//!
//! `2` is never "clean": callers must treat it as a block. The mapping from a
//! [`Verdict`] is [`exit_code`]. The one exception is `sitecraft gate`, which
//! speaks the Claude Code Stop-hook protocol (a `{"decision":"block",…}` JSON
//! line on stdout) instead.
//!
//! # Environment (test seams)
//!
//! | variable | used for | default |
//! |----------|----------|---------|
//! | [`ENV_NODE`] `SITECRAFT_NODE` | the `node` executable `check pages` spawns | `node` on `PATH` |
//! | [`ENV_NPM`] `SITECRAFT_NPM` | the `npm` executable `check security` spawns for `npm audit` | `npm` on `PATH` |
//! | [`ENV_STATE_DIR`] `SITECRAFT_STATE_DIR` | where the trust store and dev-server state files live | `$HOME/.sitecraft` |
//! | [`ENV_MARKETPLACE`] `SITECRAFT_MARKETPLACE` | path to the `marketplace.json` that `decide classify` scans for existing dedicated plugins | none (unset = undetermined) |
//!
//! # Three answers, not two
//!
//! Results are [`harness_core::verdict::Verdict`] (gate judgments) or
//! [`harness_core::verdict::Determination`] (observed values). "Could not
//! determine" is always its own answer and resolves to the restricted side.
//!
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::ffi::OsString;
use std::path::PathBuf;

pub use harness_core::verdict::{Determination, Verdict};

pub mod check;
pub mod config;
pub mod decide;
pub mod gate;
pub mod serve;
pub mod trust;

/// Env var naming the `node` executable used by `check pages`.
pub const ENV_NODE: &str = "SITECRAFT_NODE";
/// Env var naming the `npm` executable used by `check security`.
pub const ENV_NPM: &str = "SITECRAFT_NPM";
/// Env var naming the state directory (trust store, dev-server state).
pub const ENV_STATE_DIR: &str = "SITECRAFT_STATE_DIR";
/// Env var naming the `marketplace.json` scanned by `decide classify`.
pub const ENV_MARKETPLACE: &str = "SITECRAFT_MARKETPLACE";

/// Exit code: clean.
pub const EXIT_CLEAN: i32 = 0;
/// Exit code: violation found.
pub const EXIT_VIOLATION: i32 = 1;
/// Exit code: undetermined (never clean).
pub const EXIT_UNDETERMINED: i32 = 2;

/// Map a verdict to the CLI exit-code convention: `Clean` → 0,
/// `Violation` → 1, `Undetermined` → 2. Total; no verdict maps to 0 except
/// `Clean`.
#[must_use]
pub fn exit_code(v: &Verdict) -> i32 {
    match v {
        Verdict::Clean(_) => EXIT_CLEAN,
        Verdict::Violation(_) => EXIT_VIOLATION,
        Verdict::Undetermined(_) => EXIT_UNDETERMINED,
    }
}

/// Print a verdict for a CLI subcommand (reason on stderr, prefixed by
/// `label`) and return its exit code per [`exit_code`].
#[must_use]
pub fn report(label: &str, v: &Verdict) -> i32 {
    match v {
        Verdict::Clean(_) => eprintln!("{label}: clean"),
        Verdict::Violation(r) => eprintln!("{label}: VIOLATION - {}", r.as_str()),
        Verdict::Undetermined(u) => eprintln!("{label}: UNDETERMINED - {}", u.as_str()),
    }
    exit_code(v)
}

/// The state directory from the process environment: `$SITECRAFT_STATE_DIR`
/// if set and non-empty, else `$HOME/.sitecraft`. Neither available →
/// `Undetermined` (never a guessed temp dir: the trust store must not be
/// silently relocated).
pub fn state_dir() -> Determination<PathBuf> {
    state_dir_from(std::env::var_os(ENV_STATE_DIR), std::env::var_os("HOME"))
}

/// Pure core of [`state_dir`]: the same rule over explicit values.
pub fn state_dir_from(state: Option<OsString>, home: Option<OsString>) -> Determination<PathBuf> {
    if let Some(s) = state.filter(|s| !s.is_empty()) {
        return Determination::known(PathBuf::from(s));
    }
    match home.filter(|h| !h.is_empty()) {
        Some(h) => Determination::known(PathBuf::from(h).join(".sitecraft")),
        None => Determination::undetermined(format!(
            "cannot locate the sitecraft state dir: neither {ENV_STATE_DIR} nor HOME is set"
        )),
    }
}

/// The program to run for `name`: the value of env var `env` if set and
/// non-empty, else `default` (resolved on `PATH` at spawn time).
#[must_use]
pub fn program_from_env(env: &str, default: &str) -> OsString {
    match std::env::var_os(env) {
        Some(p) if !p.is_empty() => p,
        _ => OsString::from(default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_three_valued() {
        assert_eq!(exit_code(&Verdict::from_findings(vec![])), 0);
        assert_eq!(exit_code(&Verdict::violation("x")), 1);
        assert_eq!(exit_code(&Verdict::undetermined("x")), 2);
    }

    #[test]
    fn state_dir_prefers_env_then_home_then_undetermined() {
        assert_eq!(
            state_dir_from(Some("/s".into()), Some("/h".into())),
            Determination::known(PathBuf::from("/s"))
        );
        assert_eq!(
            state_dir_from(Some("".into()), Some("/h".into())),
            Determination::known(PathBuf::from("/h/.sitecraft"))
        );
        assert!(matches!(
            state_dir_from(None, None),
            Determination::Undetermined(_)
        ));
    }
}
