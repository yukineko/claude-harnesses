//! Configuration: project `gauge.toml` (preferred) over a home-level
//! `~/.gauge/config.toml` over built-in defaults. The first file that exists
//! wins (the layers are not merged), matching the rest of the toolkit.
//!
//! Loading is three-valued ([`Config::load_checked`]): no config file at all is
//! `Known(defaults)`; a config file that exists but cannot be read or parsed,
//! or whose `[[pricing]]` row cannot be applied as written (a missing or
//! misspelled rate field), is `Undetermined`. The old loader collapsed both
//! into defaults and mapped a missing rate to `0.0` — a one-letter typo in an
//! override then priced every matching model at $0 and beat the built-in table
//! (backlog 00880b3d). Same split as budgetguard's `Config::load_checked`.

use std::path::{Path, PathBuf};

use harness_core::verdict::Determination;
use serde::Deserialize;

/// A per-model price override (USD per 1M input/output tokens). `pattern` is
/// matched as a substring against the model id; the first match wins.
///
/// Re-export shim, not the real impl: the pricing type and its matching logic
/// live in `harness_core::pricing`. Re-exported here only so gauge's existing
/// call sites (`config::PriceOverride`) keep working — edit it in harness-core.
pub use harness_core::pricing::PriceOverride;

#[derive(Debug, Clone)]
pub struct Config {
    pub enabled: bool,
    /// Record per-tool call counts (Bash, Edit, …).
    pub track_tools: bool,
    /// Where per-session records live.
    pub state_dir: PathBuf,
    /// Optional pricing overrides, in priority order.
    pub pricing: Vec<PriceOverride>,
}

/// One `[[pricing]]` row. Unknown keys are rejected (`inputt = 15.0` is a
/// parse error, not an ignored key next to a missing rate).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FilePrice {
    pattern: Option<String>,
    input: Option<f64>,
    output: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    enabled: Option<bool>,
    track_tools: Option<bool>,
    state_dir: Option<String>,
    pricing: Option<Vec<FilePrice>>,
}

/// The `~/.gauge` base directory. Thin wrapper over the shared helper.
pub fn base_dir() -> PathBuf {
    harness_core::config::base_dir("gauge")
}

// Re-exported so existing `crate::config::expand_tilde` call sites keep working.
pub use harness_core::config::expand_tilde;

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            track_tools: true,
            state_dir: base_dir().join("store"),
            pricing: Vec::new(),
        }
    }
}

impl Config {
    pub fn project_path(root: &Path) -> PathBuf {
        root.join("gauge.toml")
    }

    pub fn home_path() -> PathBuf {
        base_dir().join("config.toml")
    }

    /// Which config file applies: the project's `gauge.toml`, else the home
    /// fallback, else none. `try_exists`, not `exists`: "could not tell" is
    /// undetermined, not "no config".
    fn locate(root: &Path) -> Determination<Option<PathBuf>> {
        for p in [Config::project_path(root), Config::home_path()] {
            match p.try_exists() {
                Ok(true) => return Determination::Known(Some(p)),
                Ok(false) => {}
                Err(e) => {
                    return Determination::undetermined(format!(
                        "gauge config: could not determine whether {} exists: {e}",
                        p.display()
                    ))
                }
            }
        }
        Determination::Known(None)
    }

    /// Load the effective config.
    ///
    /// * `Known(defaults)` — no config file exists (the documented default
    ///   install).
    /// * `Known(cfg)` — a config file was read and every setting in it applied.
    /// * `Undetermined` — a config file exists but could not be read or parsed,
    ///   or a `[[pricing]]` row lacks `pattern`/`input`/`output`. The operator
    ///   configured *something* we cannot apply; any cost computed without it
    ///   would be a number from a config nobody wrote. The reason names the
    ///   file.
    pub fn load_checked(root: &Path) -> Determination<Self> {
        let mut cfg = Config::default();
        let path = match Config::locate(root) {
            Determination::Known(Some(p)) => p,
            Determination::Known(None) => return Determination::Known(cfg),
            Determination::Undetermined(why) => return Determination::Undetermined(why),
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                return Determination::undetermined(format!(
                    "gauge config {} exists but could not be read: {e}",
                    path.display()
                ))
            }
        };
        let fc = match toml::from_str::<FileConfig>(&text) {
            Ok(fc) => fc,
            Err(e) => {
                return Determination::undetermined(format!(
                    "gauge config {} could not be parsed: {e}",
                    path.display()
                ))
            }
        };
        if let Some(v) = fc.enabled {
            cfg.enabled = v;
        }
        if let Some(v) = fc.track_tools {
            cfg.track_tools = v;
        }
        if let Some(v) = fc.state_dir {
            cfg.state_dir = expand_tilde(&v);
        }
        if let Some(rows) = fc.pricing {
            let mut out = Vec::with_capacity(rows.len());
            for (i, r) in rows.into_iter().enumerate() {
                let n = i + 1;
                let pattern = r
                    .pattern
                    .map(|p| p.trim().to_lowercase())
                    .unwrap_or_default();
                if pattern.is_empty() {
                    return Determination::undetermined(format!(
                        "gauge config {}: [[pricing]] row {n} has no (or an empty) `pattern`",
                        path.display()
                    ));
                }
                let (Some(input), Some(output)) = (r.input, r.output) else {
                    return Determination::undetermined(format!(
                        "gauge config {}: [[pricing]] row {n} (pattern {pattern:?}) is missing \
                         its `input` and/or `output` rate; a missing rate is not $0",
                        path.display()
                    ));
                };
                out.push(PriceOverride {
                    pattern,
                    input,
                    output,
                });
            }
            cfg.pricing = out;
        }
        Determination::Known(cfg)
    }

    pub fn disabled_env() -> bool {
        std::env::var("GAUGE_DISABLE")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false)
    }

    pub fn config_source(root: &Path) -> PathBuf {
        let p = Config::project_path(root);
        if p.exists() {
            return p;
        }
        let h = Config::home_path();
        if h.exists() {
            return h;
        }
        PathBuf::from("(defaults — no config file)")
    }
}
