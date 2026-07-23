//! Configuration: project `donegate.toml` (preferred) layered over a home-level
//! `~/.donegate/config.toml`, over built-in defaults. Env overrides last.
//!
//! Safe by default: with no config and no checks, the gate runs nothing and lets
//! every stop through — installing the hook can never *block* a project that
//! hasn't opted in with at least one `[[check]]`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Deserialize;

// Re-exported so existing `crate::config::expand_tilde` call sites keep working.
pub use harness_core::config::expand_tilde;
use harness_core::trust;

/// One acceptance command, run as a subprocess on Stop.
#[derive(Debug, Clone, Deserialize)]
pub struct Check {
    /// Short label shown in the block reason (e.g. "test", "clippy").
    pub name: String,
    /// Shell command line; run via `sh -c` (Unix) / `cmd /C` (Windows).
    pub cmd: String,
    /// If set, the check runs only when a changed file (git diff vs HEAD +
    /// untracked) matches one of these globs. Absent ⇒ always run.
    #[serde(default)]
    pub when_changed: Option<Vec<String>>,
    /// Per-check timeout; falls back to `default_timeout_secs`.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// A failing optional check warns but never blocks the stop.
    #[serde(default)]
    pub optional: bool,
    /// Run the command in this subdir of the project root.
    #[serde(default)]
    pub workdir: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub enabled: bool,
    /// After this many consecutive blocks in one session, give up and allow the
    /// stop (so a genuinely stuck agent isn't trapped forever).
    pub max_attempts: u32,
    pub default_timeout_secs: u64,
    /// How many trailing lines of a failing command's output to feed back.
    pub output_tail_lines: usize,
    /// A session's attempt counter resets if this many seconds pass between
    /// stops (a fresh turn after the user did other work).
    pub reset_after_secs: i64,
    pub state_dir: PathBuf,
    pub checks: Vec<Check>,
}

/// On-disk form; every field optional.
#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    enabled: Option<bool>,
    max_attempts: Option<u32>,
    default_timeout_secs: Option<u64>,
    output_tail_lines: Option<usize>,
    reset_after_secs: Option<i64>,
    state_dir: Option<String>,
    #[serde(default)]
    check: Vec<Check>,
}

/// The `~/.donegate` base directory. Thin wrapper over the shared primitive.
pub fn base_dir() -> PathBuf {
    harness_core::config::base_dir("donegate")
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            max_attempts: 3,
            default_timeout_secs: 300,
            output_tail_lines: 40,
            reset_after_secs: 600,
            state_dir: base_dir().join("state"),
            checks: Vec::new(),
        }
    }
}

impl Config {
    /// The project config path (`<root>/donegate.toml`).
    pub fn project_path(root: &Path) -> PathBuf {
        root.join("donegate.toml")
    }

    /// The home config path (`~/.donegate/config.toml`).
    pub fn home_path() -> PathBuf {
        base_dir().join("config.toml")
    }

    /// Load config for a project root. A project `donegate.toml` wins outright —
    /// **but only when the project root is trusted** (`harness_core::trust`),
    /// because a project `[[check]].cmd` is later run via `sh -c` and a hostile
    /// repository could otherwise ship arbitrary commands. An untrusted project
    /// file is ignored (treated as absent), falling back to the home config;
    /// otherwise built-in defaults. Any parse error silently falls back (the gate
    /// must never crash a turn).
    pub fn load(root: &Path) -> Self {
        let mut cfg = Config::default();

        let chosen = {
            let p = Config::project_path(root);
            if p.exists() {
                if trust::is_trusted(root) {
                    Some(p)
                } else {
                    warn_untrusted_once(root, &p);
                    // Untrusted project config carries unvetted commands; ignore
                    // it and fall back to the (trusted) home config / defaults.
                    let h = Config::home_path();
                    if h.exists() {
                        Some(h)
                    } else {
                        None
                    }
                }
            } else {
                let h = Config::home_path();
                if h.exists() {
                    Some(h)
                } else {
                    None
                }
            }
        };

        if let Some(path) = chosen {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(fc) = toml::from_str::<FileConfig>(&text) {
                    if let Some(v) = fc.enabled {
                        cfg.enabled = v;
                    }
                    if let Some(v) = fc.max_attempts {
                        cfg.max_attempts = v;
                    }
                    if let Some(v) = fc.default_timeout_secs {
                        cfg.default_timeout_secs = v;
                    }
                    if let Some(v) = fc.output_tail_lines {
                        cfg.output_tail_lines = v;
                    }
                    if let Some(v) = fc.reset_after_secs {
                        cfg.reset_after_secs = v;
                    }
                    if let Some(v) = fc.state_dir {
                        cfg.state_dir = expand_tilde(&v);
                    }
                    cfg.checks = fc.check;
                }
            }
        }

        // sanitize
        if cfg.max_attempts == 0 {
            cfg.max_attempts = 1;
        }
        if cfg.default_timeout_secs == 0 {
            cfg.default_timeout_secs = 300;
        }
        if cfg.output_tail_lines == 0 {
            cfg.output_tail_lines = 40;
        }
        cfg.checks
            .retain(|c| !c.name.trim().is_empty() && !c.cmd.trim().is_empty());
        cfg
    }

    /// Globally disabled via env.
    pub fn disabled_env() -> bool {
        std::env::var("DONEGATE_DISABLE")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false)
    }
}

/// Emit a one-shot notice (per process) that a project config was ignored
/// because the project isn't trusted. Best effort — never panics, never blocks.
fn warn_untrusted_once(root: &Path, project_path: &Path) {
    static WARNED: AtomicBool = AtomicBool::new(false);
    if WARNED.swap(true, Ordering::Relaxed) {
        return;
    }
    eprintln!(
        "donegate: {} provides commands but this project is not trusted; ignoring it. \
         Run 'donegate trust' to enable.",
        project_path.display()
    );
    let _ = root;
}

/// Serializes every test in this crate that mutates the process-global
/// `$HOME` env var (workspace-trust lookups here, and — since main.rs's
/// `violation_emission_tests` also point `$HOME` at a scratch dir to isolate
/// `overwatch::store`'s home-relative storage root — the violation-emission
/// tests too). `cargo test` runs a crate's unit tests on multiple threads by
/// default; two such tests racing without a shared lock corrupts each
/// other's view of "which project is trusted" / "where does the store
/// live" (observed directly: adding the violation-emission tests without
/// wiring them to this lock made `project_config_is_gated_behind_workspace_trust`
/// fail intermittently under the default multi-threaded test runner).
#[cfg(test)]
pub(crate) static HOME_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    const PROJECT_TOML: &str = r#"
[[check]]
name = "proj"
cmd = "echo project"
"#;

    const HOME_TOML: &str = r#"
[[check]]
name = "homecheck"
cmd = "echo home"
"#;

    // Mutates process-global HOME / HARNESS_TRUST_ALL, so the whole trust-gate
    // scenario runs in a single serialized #[test].
    #[test]
    fn project_config_is_gated_behind_workspace_trust() {
        let _guard = HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        std::env::remove_var("HARNESS_TRUST_ALL");

        let root = proj.path();
        write(&Config::project_path(root), PROJECT_TOML);

        // 1. Untrusted project + no home config ⇒ project checks are NOT loaded;
        //    we fall through to built-in defaults (which have no checks).
        let cfg = Config::load(root);
        assert!(
            cfg.checks.is_empty(),
            "untrusted project config must not contribute checks"
        );

        // 2. Untrusted project but a home config exists ⇒ home checks win, the
        //    project's checks are still ignored.
        write(&Config::home_path(), HOME_TOML);
        let cfg = Config::load(root);
        assert_eq!(
            cfg.checks
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["homecheck"],
            "untrusted project must fall back to the home config"
        );

        // 3. Trust the project ⇒ project config now wins outright (as before).
        trust::add(root).unwrap();
        let cfg = Config::load(root);
        assert_eq!(
            cfg.checks
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["proj"],
            "trusted project config must win over the home config"
        );

        // 4. Removing trust reverts to the home fallback.
        assert!(trust::remove(root).unwrap());
        let cfg = Config::load(root);
        assert_eq!(
            cfg.checks
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["homecheck"],
            "untrusting must drop the project config again"
        );

        // 5. HARNESS_TRUST_ALL is the global escape hatch: project wins without
        //    an explicit trust entry.
        std::env::set_var("HARNESS_TRUST_ALL", "1");
        let cfg = Config::load(root);
        assert_eq!(
            cfg.checks
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["proj"],
            "HARNESS_TRUST_ALL must honor the project config"
        );
        std::env::remove_var("HARNESS_TRUST_ALL");

        // 6. Home config needs no trust at all: a project with NO donegate.toml
        //    loads the home checks directly.
        let bare = tempfile::tempdir().unwrap();
        let cfg = Config::load(bare.path());
        assert_eq!(
            cfg.checks
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["homecheck"],
            "home config must load without any trust"
        );
    }
}
