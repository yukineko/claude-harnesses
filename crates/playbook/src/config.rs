//! Configuration: project `playbook.toml` over `~/.playbook/config.toml` over
//! defaults, with an env kill switch. Retrieval only ever *injects* short notes
//! under a char budget for scored notes (`always` notes are injected in full
//! past it) — it can never block a turn.

use std::path::{Path, PathBuf};

use serde::Deserialize;

// Re-exported so existing `crate::config::expand_tilde` call sites keep working.
pub use harness_core::config::expand_tilde;

#[derive(Debug, Clone)]
pub struct Config {
    pub enabled: bool,
    pub store_dir: PathBuf,
    /// Max scored notes injected per prompt (`always` notes do not count).
    pub top_k: usize,
    /// Minimum relevance score for a note to be injected (always-notes bypass).
    pub min_score: i64,
    /// Char budget for scored notes: `always` notes are injected in full past
    /// it; an overflowing scored note is dropped unless it is the first admitted.
    pub max_chars: usize,
    /// Also search the shared `_global` store, not just the project's notes.
    pub include_global: bool,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    enabled: Option<bool>,
    store_dir: Option<String>,
    top_k: Option<usize>,
    min_score: Option<i64>,
    max_chars: Option<usize>,
    include_global: Option<bool>,
}

/// The `~/.playbook` base directory (thin wrapper over the shared helper).
pub fn base_dir() -> PathBuf {
    harness_core::config::base_dir("playbook")
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            store_dir: base_dir().join("store"),
            top_k: 3,
            min_score: 5,
            max_chars: 1500,
            include_global: true,
        }
    }
}

impl Config {
    pub fn project_path(root: &Path) -> PathBuf {
        root.join("playbook.toml")
    }
    pub fn home_path() -> PathBuf {
        base_dir().join("config.toml")
    }

    pub fn load(root: &Path) -> Self {
        let mut cfg = Config::default();
        // 3-layer resolution (project file → global file → defaults) lives in
        // harness_core; this crate only owns how its own fields merge on top.
        let fc = harness_core::inject::load_layered::<FileConfig>(
            &Config::project_path(root),
            &Config::home_path(),
        );
        if let Some(v) = fc.enabled {
            cfg.enabled = v;
        }
        if let Some(v) = fc.store_dir {
            cfg.store_dir = expand_tilde(&v);
        }
        if let Some(v) = fc.top_k {
            cfg.top_k = v;
        }
        if let Some(v) = fc.min_score {
            cfg.min_score = v;
        }
        if let Some(v) = fc.max_chars {
            cfg.max_chars = v;
        }
        if let Some(v) = fc.include_global {
            cfg.include_global = v;
        }
        cfg.top_k = cfg.top_k.max(1);
        cfg.max_chars = cfg.max_chars.max(120);
        cfg
    }

    pub fn disabled_env() -> bool {
        std::env::var("PLAYBOOK_DISABLE")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false)
    }
}
