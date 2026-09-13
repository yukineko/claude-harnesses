//! The ONE autonomy switch, read directly by every plugin.
//!
//! # Why this exists
//!
//! Continuous autonomy needs several switches to agree, and before this module
//! each crate owned its own and none of them consulted the others:
//!
//!   - condukt: `~/.condukt/config.toml` `autonomous` + `CONDUKT_AUTONOMOUS`;
//!   - ctxrot: `auto_distill_on_band` / `auto_compact_enabled`, two independent
//!     bools that never asked condukt anything;
//!   - autoflow: shelled out to `condukt state autonomy-check`, so a `condukt`
//!     missing from `PATH` was indistinguishable from "not autonomous".
//!
//! Turning one on did not turn the others on. This module is the shared layer
//! all of them now read — **in-process, no subprocess**, so a missing binary or
//! a `PATH` difference can never masquerade as a verdict.
//!
//! # Resolution order (highest first)
//!
//! 1. env `HARNESS_AUTONOMOUS`
//! 2. env `CONDUKT_AUTONOMOUS` (legacy alias; kept working verbatim)
//! 3. the durable switch file (see [`switch_path`])
//! 4. the calling crate's own config value (`config_value`)
//! 5. default: off
//!
//! # Fail-closed, and NOT silently (CLAUDE.md §3 / §1)
//!
//! Two different things must never collapse onto the same observable behaviour:
//!
//!   - the switch file is **absent** — a determinate "never set". Resolves to
//!     off via [`Source::BuiltinDefault`] (or [`Source::Config`] when the crate has a
//!     config value). Nothing is wrong, so nothing is warned about.
//!   - the switch file **exists but cannot be read or parsed** — that is
//!     [`Determination::Undetermined`], surfaced as
//!     [`Source::UndeterminedSwitchFile`]. Every consumer resolves it to OFF
//!     (the human stays in the loop) **and** emits a visible warning naming the
//!     file. A corrupt switch that behaved byte-identically to "never set"
//!     would be exactly the silence-reads-as-no-problem fail-open this module
//!     exists to prevent.
//!
//! [`resolve`] performs that collapse once, in one place, and hands the caller
//! the warning text to surface, so no consumer has to re-derive either half.

use crate::verdict::Determination;
use std::path::{Path, PathBuf};

/// The durable answer the switch file records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutonomyMode {
    On,
    Off,
}

impl AutonomyMode {
    /// `"on"` / `"off"` — the exact token written to (and read from) the file.
    pub fn as_str(&self) -> &'static str {
        match self {
            AutonomyMode::On => "on",
            AutonomyMode::Off => "off",
        }
    }

    pub fn is_on(&self) -> bool {
        matches!(self, AutonomyMode::On)
    }

    /// Parse the `"on"`/`"off"` token. Anything else is `None` — an unknown
    /// mode is not "off", it is unreadable, and the caller turns it into
    /// `Undetermined`.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "on" => Some(AutonomyMode::On),
            "off" => Some(AutonomyMode::Off),
            _ => None,
        }
    }
}

impl From<bool> for AutonomyMode {
    fn from(b: bool) -> Self {
        if b {
            AutonomyMode::On
        } else {
            AutonomyMode::Off
        }
    }
}

/// WHICH layer decided. Reported by `condukt state autonomy-check --explain`,
/// `condukt state autonomy-path` and `ctxrot autonomy` so an operator can see
/// why they are (or are not) autonomous instead of guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `HARNESS_AUTONOMOUS` or `CONDUKT_AUTONOMOUS`.
    Env,
    /// The durable switch file.
    SwitchFile,
    /// The calling crate's own config value.
    Config,
    /// Nothing was set anywhere: off.
    ///
    /// NAMED `BuiltinDefault`, not `Default`: a public item literally named
    /// `Default` in this crate makes rustc's path-trimming give up on the name
    /// crate-wide, so EVERY downstream diagnostic mentioning the `Default`
    /// trait starts printing `std::default::Default` instead. That is not
    /// hypothetical — it reddened the frozen UI fixture
    /// `crates/harness-core/tests/ui/verdict/default_verdict.stderr`, which
    /// pins the exact compiler text of the "`Verdict` has no `Default`"
    /// refusal. The JSON wire token is still `"default"` (see `as_str`).
    BuiltinDefault,
    /// The switch file exists but could not be read/parsed, or its path could
    /// not be resolved. Distinct from `BuiltinDefault` on purpose.
    UndeterminedSwitchFile,
}

impl Source {
    /// The wire token used in every JSON surface.
    pub fn as_str(&self) -> &'static str {
        match self {
            Source::Env => "env",
            Source::SwitchFile => "switch-file",
            Source::Config => "config",
            Source::BuiltinDefault => "default",
            Source::UndeterminedSwitchFile => "undetermined-switch-file",
        }
    }
}

/// Parse an autonomy env override. Accepts the same truthy/falsy spellings
/// `CONDUKT_AUTONOMOUS` has always accepted; an unrecognized value is `None`,
/// which means "this layer says nothing" and falls through to the next layer
/// (it does NOT mean off).
fn parse_env(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn env_override() -> Option<bool> {
    for key in ["HARNESS_AUTONOMOUS", "CONDUKT_AUTONOMOUS"] {
        if let Ok(v) = std::env::var(key) {
            if let Some(b) = parse_env(&v) {
                return Some(b);
            }
        }
    }
    None
}

/// The directory the switch files live in: `$HARNESS_AUTONOMY_DIR` when set and
/// non-empty, else `$HOME/.harness/autonomy`.
pub fn switch_dir() -> PathBuf {
    if let Ok(v) = std::env::var("HARNESS_AUTONOMY_DIR") {
        if !v.trim().is_empty() {
            return PathBuf::from(v);
        }
    }
    crate::config::home().join(".harness").join("autonomy")
}

/// `<switch_dir()>/<project-key>.json` for the repo containing `cwd`.
///
/// The key is derived from [`crate::projkey::main_worktree_root`], **not**
/// [`crate::projkey::repo_root`]. Under CLAUDE.md §8 every worker runs inside a
/// linked worktree, and `repo_root` stops at the worktree itself — so a
/// `repo_root`-keyed file would give each worktree its own switch and the one
/// the human set in the main tree would be invisible to the workers. A switch a
/// worker cannot see is not a switch.
///
/// `Undetermined` is FORWARDED verbatim from `main_worktree_root` (not
/// re-minted: re-minting would double-record the same undetermined event in the
/// telemetry ledger) and is never collapsed onto a `repo_root` guess.
pub fn switch_path(cwd: &Path) -> Determination<PathBuf> {
    match crate::projkey::main_worktree_root(cwd) {
        Determination::Known(root) => Determination::known(
            switch_dir().join(format!("{}.json", crate::projkey::project_key(&root))),
        ),
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

/// Resolve autonomy through every layer, reporting WHICH layer answered.
///
/// `config_value` is the calling crate's own config setting (condukt's
/// `config.toml` `autonomous`; `None` for a crate that has no such setting). It
/// sits BELOW the switch file, so the shared switch overrides a per-crate
/// config but is itself overridden by the environment.
///
/// An absent switch file is `Known` (a determinate "never set") and falls
/// through to `config_value`/default. A switch file that exists but cannot be
/// read or parsed is `Undetermined` with [`Source::UndeterminedSwitchFile`];
/// see [`resolve`] for the fail-closed collapse every consumer applies.
pub fn read(cwd: &Path, config_value: Option<bool>) -> (Determination<AutonomyMode>, Source) {
    // Layer 1+2: the environment outranks the durable switch, so a switch that
    // was left on can always be forced off for one invocation.
    if let Some(b) = env_override() {
        return (Determination::known(AutonomyMode::from(b)), Source::Env);
    }

    // Layer 3: the durable switch file.
    let path = match switch_path(cwd) {
        Determination::Known(p) => p,
        // The switch's own address could not be resolved; forward the reason.
        Determination::Undetermined(why) => {
            return (
                Determination::Undetermined(why),
                Source::UndeterminedSwitchFile,
            )
        }
    };
    match crate::boundary::read_to_string(&path) {
        Determination::Known(Some(text)) => match parse_switch_file(&text) {
            Some(mode) => return (Determination::known(mode), Source::SwitchFile),
            None => {
                return (
                    Determination::undetermined(format!(
                        "autonomy: switch-file-unparseable: {} exists but names no \
                         `\"mode\": \"on\"|\"off\"`; treating it as unset would make a \
                         BROKEN switch indistinguishable from one that was never set",
                        path.display()
                    )),
                    Source::UndeterminedSwitchFile,
                )
            }
        },
        // Absent: a determinate "never set", not a failure to look.
        Determination::Known(None) => {}
        // Unreadable (permissions, non-UTF-8, ...): forward the boundary's
        // reason, which already names the file.
        Determination::Undetermined(why) => {
            return (
                Determination::Undetermined(why),
                Source::UndeterminedSwitchFile,
            )
        }
    }

    // Layer 4: the crate's own config value.
    if let Some(b) = config_value {
        return (Determination::known(AutonomyMode::from(b)), Source::Config);
    }

    // Layer 5: off.
    (
        Determination::known(AutonomyMode::Off),
        Source::BuiltinDefault,
    )
}

/// The switch file's payload: `{"mode":"on"}` / `{"mode":"off"}`.
fn parse_switch_file(text: &str) -> Option<AutonomyMode> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    AutonomyMode::parse(v.get("mode")?.as_str()?)
}

/// The fail-closed collapse every consumer needs, done once here.
///
/// `autonomous` is the bool the caller acts on; `source` names the deciding
/// layer; `warning` is `Some` exactly when the answer was `Undetermined`, and
/// the caller MUST surface it (stderr) — resolving to off silently would erase
/// the difference between a broken switch and an unset one.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub autonomous: bool,
    pub source: Source,
    pub warning: Option<String>,
}

/// Resolve [`read`] to a bool, failing closed on `Undetermined`.
pub fn resolve(cwd: &Path, config_value: Option<bool>) -> Resolved {
    match read(cwd, config_value) {
        (Determination::Known(mode), source) => Resolved {
            autonomous: mode.is_on(),
            source,
            warning: None,
        },
        (Determination::Undetermined(why), source) => Resolved {
            autonomous: false,
            source,
            // Named, not silent: the path is in `why`, and the operator is told
            // both what happened and that autonomy was forced off because of it.
            warning: Some(format!(
                "warning: autonomy switch is UNDETERMINED: {why}\n\
                 warning: resolving autonomy to OFF (fail-closed) — rewrite the switch \
                 with `condukt state autonomy-set on|off`"
            )),
        },
    }
}

/// Write the durable switch atomically (temp file + rename), creating the
/// directory if needed. Atomic so a concurrent reader never sees a truncated
/// file — which would read back as `Undetermined` and force autonomy off.
pub fn write(cwd: &Path, mode: AutonomyMode) -> anyhow::Result<()> {
    let path = match switch_path(cwd) {
        Determination::Known(p) => p,
        Determination::Undetermined(why) => {
            anyhow::bail!("cannot resolve the autonomy switch file path: {why}")
        }
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    std::fs::write(&tmp, format!("{{\"mode\":\"{}\"}}\n", mode.as_str()))?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_tokens_are_the_wire_contract() {
        assert_eq!(Source::Env.as_str(), "env");
        assert_eq!(Source::SwitchFile.as_str(), "switch-file");
        assert_eq!(Source::Config.as_str(), "config");
        assert_eq!(Source::BuiltinDefault.as_str(), "default");
        assert_eq!(
            Source::UndeterminedSwitchFile.as_str(),
            "undetermined-switch-file"
        );
    }

    #[test]
    fn mode_parses_only_on_and_off() {
        assert_eq!(AutonomyMode::parse("on"), Some(AutonomyMode::On));
        assert_eq!(AutonomyMode::parse(" OFF "), Some(AutonomyMode::Off));
        assert_eq!(AutonomyMode::parse("maybe"), None);
        assert_eq!(AutonomyMode::parse(""), None);
    }

    #[test]
    fn switch_file_payload_roundtrips() {
        assert_eq!(
            parse_switch_file(r#"{"mode":"on"}"#),
            Some(AutonomyMode::On)
        );
        assert_eq!(
            parse_switch_file(r#"{"mode":"off"}"#),
            Some(AutonomyMode::Off)
        );
        // Not JSON, valid JSON with no mode, and an unknown mode are all
        // "cannot read", never "off".
        assert_eq!(parse_switch_file("garbage {{{"), None);
        assert_eq!(parse_switch_file(r#"{"other":1}"#), None);
        assert_eq!(parse_switch_file(r#"{"mode":"perhaps"}"#), None);
    }

    #[test]
    fn env_parser_leaves_unknown_values_to_the_next_layer() {
        assert_eq!(parse_env("1"), Some(true));
        assert_eq!(parse_env("off"), Some(false));
        assert_eq!(parse_env("maybe"), None);
    }
}
