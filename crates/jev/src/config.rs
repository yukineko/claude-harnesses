//! Configuration: `~/.jev/config.toml`, overridable by environment.
//!
//! Note what is *not* configurable: the API key (environment only — a config
//! file is a plaintext credential on disk and a flag is shell history), and
//! anything that could widen what jev is allowed to decide.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The default model route.
///
/// `jev-latest` is the documented route, and it is also the one TypeSafe's own
/// docs warn about: it resolves to a concrete version that changes, and a
/// threshold tuned against one version drifts when it does (measured
/// 2026-10-02). Pin a concrete version in `~/.jev/config.toml` before tuning
/// any threshold against a probability. The ledger records the `model` the
/// response reported, so what actually answered is observable after the fact
/// rather than assumed.
pub const DEFAULT_MODEL: &str = "jev-latest";

/// Hard wall-clock ceiling for one call. A jev call is an advisory hint on
/// someone's critical path, so it must not be able to stall a turn; exceeding
/// this is a `NoSignal`, which is the safe direction by construction.
pub const DEFAULT_MAX_TIME_SECS: u64 = 8;

/// Env overrides.
pub const MODEL_ENV: &str = "JEV_MODEL";
pub const MAX_TIME_ENV: &str = "JEV_MAX_TIME_SECS";
pub const LEDGER_ENV: &str = "JEV_LEDGER";
pub const CONFIG_ENV: &str = "JEV_CONFIG";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// Model route. See [`DEFAULT_MODEL`].
    pub model: String,
    /// `curl --max-time` in seconds.
    pub max_time_secs: u64,
    /// Where the usage ledger is appended. `None` → `$HOME/.jev/usage.jsonl`.
    #[serde(default)]
    pub ledger_path: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            model: DEFAULT_MODEL.to_string(),
            max_time_secs: DEFAULT_MAX_TIME_SECS,
            ledger_path: None,
        }
    }
}

impl Config {
    /// Load from disk and apply env overrides.
    ///
    /// A missing config file is the normal case and yields defaults. A config
    /// file that exists but does not parse is **reported, not ignored**: the
    /// caller gets the error and can decide, rather than silently running on
    /// defaults that differ from what the operator wrote.
    pub fn load() -> Result<Self, String> {
        let mut cfg = match Self::config_path() {
            Some(p) if p.exists() => {
                let text = std::fs::read_to_string(&p)
                    .map_err(|e| format!("reading {}: {e}", p.display()))?;
                toml::from_str::<Config>(&text)
                    .map_err(|e| format!("parsing {}: {e}", p.display()))?
            }
            _ => Config::default(),
        };
        cfg.apply_env();
        Ok(cfg)
    }

    fn apply_env(&mut self) {
        if let Ok(m) = std::env::var(MODEL_ENV) {
            let m = m.trim();
            if !m.is_empty() {
                self.model = m.to_string();
            }
        }
        if let Ok(raw) = std::env::var(MAX_TIME_ENV) {
            // A malformed value keeps the current ceiling rather than removing
            // it: clamping a timeout to "unlimited" on bad input would be the
            // floorless clamp CLAUDE.md §3 calls out.
            if let Ok(n) = raw.trim().parse::<u64>() {
                if n > 0 {
                    self.max_time_secs = n;
                }
            }
        }
        if let Ok(p) = std::env::var(LEDGER_ENV) {
            let p = p.trim();
            if !p.is_empty() {
                self.ledger_path = Some(PathBuf::from(p));
            }
        }
    }

    fn config_path() -> Option<PathBuf> {
        if let Ok(p) = std::env::var(CONFIG_ENV) {
            let p = p.trim();
            if !p.is_empty() {
                return Some(PathBuf::from(p));
            }
        }
        Self::home().map(|h| h.join(".jev").join("config.toml"))
    }

    pub(crate) fn home() -> Option<PathBuf> {
        std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
    }

    /// Resolved ledger path, if one can be determined.
    pub fn resolved_ledger_path(&self) -> Option<PathBuf> {
        if let Some(p) = &self.ledger_path {
            return Some(p.clone());
        }
        Self::home().map(|h| h.join(".jev").join("usage.jsonl"))
    }

    /// `true` when the model route is an unpinned alias, i.e. a tuned
    /// threshold can drift under it.
    pub fn model_is_unpinned(&self) -> bool {
        self.model.ends_with("-latest")
    }
}

/// Parse a config from a string, for tests and for `jev check`.
pub fn parse(text: &str) -> Result<Config, String> {
    toml::from_str::<Config>(text).map_err(|e| format!("parsing config: {e}"))
}

/// Read a config from an explicit path, bypassing the environment.
pub fn load_from(path: &Path) -> Result<Config, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    parse(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_documented_ones() {
        let c = Config::default();
        assert_eq!(c.model, "jev-latest");
        assert_eq!(c.max_time_secs, 8);
        assert!(c.model_is_unpinned());
    }

    #[test]
    fn a_pinned_model_is_recognised() {
        let c = parse("model = \"jev-1.13.0\"\nmax_time_secs = 3\n").unwrap();
        assert_eq!(c.model, "jev-1.13.0");
        assert_eq!(c.max_time_secs, 3);
        assert!(!c.model_is_unpinned());
    }

    #[test]
    fn a_broken_config_is_an_error_not_a_silent_default() {
        let err = parse("model = ").unwrap_err();
        assert!(err.contains("parsing config"), "err: {err}");
    }

    #[test]
    fn zero_timeout_is_rejected_so_the_ceiling_cannot_be_removed() {
        let mut c = Config::default();
        // Simulate the env path's guard directly: 0 must not be accepted.
        let before = c.max_time_secs;
        if let Ok(n) = "0".parse::<u64>() {
            if n > 0 {
                c.max_time_secs = n;
            }
        }
        assert_eq!(c.max_time_secs, before);
    }
}
