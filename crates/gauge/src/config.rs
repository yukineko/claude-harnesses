//! Configuration: project `gauge.toml` (preferred) over a home-level
//! `~/.gauge/config.toml` over built-in defaults. The first file that exists
//! wins (the layers are not merged), matching the rest of the toolkit.

use std::path::{Path, PathBuf};

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

#[derive(Debug, Default, Deserialize)]
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

    pub fn load(root: &Path) -> Self {
        let mut cfg = Config::default();
        let chosen = {
            let p = Config::project_path(root);
            if p.exists() {
                Some(p)
            } else {
                let h = Config::home_path();
                h.exists().then_some(h)
            }
        };
        if let Some(path) = chosen {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(fc) = toml::from_str::<FileConfig>(&text) {
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
                        cfg.pricing = rows
                            .into_iter()
                            .filter_map(|r| {
                                let pattern = r.pattern?.trim().to_lowercase();
                                if pattern.is_empty() {
                                    return None;
                                }
                                Some(PriceOverride {
                                    pattern,
                                    input: r.input.unwrap_or(0.0),
                                    output: r.output.unwrap_or(0.0),
                                })
                            })
                            .collect();
                    }
                }
            }
        }
        cfg
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
