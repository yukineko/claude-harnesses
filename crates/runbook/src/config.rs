//! Configuration: project `runbook.toml` over `~/.runbook/config.toml` over
//! built-in defaults (first file that exists wins; layers are not merged). Env
//! kill switch `RUNBOOK_DISABLE`. The hook only ever *injects* the procedures a
//! prompt explicitly asked for under a hard char budget — it can never block a
//! turn.

use std::path::{Path, PathBuf};

use serde::Deserialize;

// Re-exported so existing `crate::config::expand_tilde` call sites keep working.
pub use harness_core::config::expand_tilde;

#[derive(Debug, Clone)]
pub struct Config {
    pub enabled: bool,
    /// Project-relative directory holding `<name>.md` procedures.
    pub project_dir: String,
    /// Absolute directory for shared, cross-project procedures.
    pub global_dir: PathBuf,
    /// Also resolve `!name` against the global directory.
    pub include_global: bool,
    /// The macro prefix (default `!`).
    pub prefix: char,
    /// `!<index_token>` injects the list of available runbooks instead of a body.
    pub index_token: String,
    /// Hard cap on total injected characters across all expanded macros.
    pub max_chars: usize,
    /// Per-runbook truncation cap.
    pub per_runbook_chars: usize,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    enabled: Option<bool>,
    project_dir: Option<String>,
    global_dir: Option<String>,
    include_global: Option<bool>,
    prefix: Option<String>,
    index_token: Option<String>,
    max_chars: Option<usize>,
    per_runbook_chars: Option<usize>,
}

/// The `~/.runbook` base directory (preserves the existing on-disk dir name).
pub fn base_dir() -> PathBuf {
    harness_core::config::base_dir("runbook")
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            project_dir: ".runbook".to_string(),
            global_dir: base_dir().join("runbooks"),
            include_global: true,
            prefix: '!',
            index_token: "runbooks".to_string(), // audit-ignore: macro token name, not a secret/credential
            max_chars: 12000,
            per_runbook_chars: 4000,
        }
    }
}

impl Config {
    pub fn project_path(root: &Path) -> PathBuf {
        root.join("runbook.toml")
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
        if let Some(v) = fc.project_dir {
            cfg.project_dir = v;
        }
        if let Some(v) = fc.global_dir {
            cfg.global_dir = expand_tilde(&v);
        }
        if let Some(v) = fc.include_global {
            cfg.include_global = v;
        }
        if let Some(v) = fc.prefix {
            if let Some(c) = v.chars().next() {
                cfg.prefix = c;
            }
        }
        if let Some(v) = fc.index_token {
            if !v.trim().is_empty() {
                cfg.index_token = v.trim().to_lowercase();
            }
        }
        if let Some(v) = fc.max_chars {
            cfg.max_chars = v;
        }
        if let Some(v) = fc.per_runbook_chars {
            cfg.per_runbook_chars = v;
        }
        cfg.max_chars = cfg.max_chars.max(200);
        cfg.per_runbook_chars = cfg.per_runbook_chars.clamp(200, cfg.max_chars);
        cfg
    }

    /// Resolve the project procedure directory against a project root.
    pub fn project_runbook_dir(&self, root: &Path) -> PathBuf {
        expand_or_join(root, &self.project_dir)
    }

    pub fn disabled_env() -> bool {
        std::env::var("RUNBOOK_DISABLE")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false)
    }
}

/// `~`-expand an absolute-ish path, else join it onto `root`.
fn expand_or_join(root: &Path, dir: &str) -> PathBuf {
    if dir.starts_with('~') {
        expand_tilde(dir)
    } else {
        let p = PathBuf::from(dir);
        if p.is_absolute() {
            p
        } else {
            root.join(p)
        }
    }
}
