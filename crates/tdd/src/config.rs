//! Configuration: project `tdd.toml` (preferred) layered over a home-level
//! `~/.tdd/config.toml`, over built-in defaults.
//!
//! Safe by default: if `enabled = false` (or the env kill-switch is set) the
//! Stop gate allows every stop. The built-in defaults are language-aware so a
//! Rust/Python/TS/Go project works with no config at all.

use std::path::{Path, PathBuf};

use serde::Deserialize;

pub use harness_core::config::expand_tilde;
use harness_core::trust;

#[derive(Debug, Clone)]
pub struct Config {
    pub enabled: bool,
    /// After this many consecutive blocks in one session, give up and allow the
    /// stop (so a genuinely stuck agent isn't trapped forever).
    pub max_attempts: u32,
    /// A session's attempt counter resets if this many seconds pass between
    /// stops (a fresh turn after the user did other work).
    pub reset_after_secs: i64,
    pub state_dir: PathBuf,
    /// Directory (relative to the project root) where RED/GREEN proof artifacts
    /// are written by `tdd red` / `tdd green`.
    pub proof_dir: String,
    /// Default test command for `tdd red` / `tdd green` when `--cmd` is omitted.
    pub test_cmd: String,
    pub default_timeout_secs: u64,
    pub output_tail_lines: usize,
    /// Globs for files that count as *implementation*.
    pub impl_globs: Vec<String>,
    /// Globs for files that are tests *by location/name* (these never count as
    /// implementation, and changing one is test evidence).
    pub test_path_globs: Vec<String>,
    /// Regexes matched against *added* diff lines to detect an inline test was
    /// written (e.g. `#[test]`, `def test_`, `func TestX`, `it(`).
    pub test_markers: Vec<String>,
    /// Block only when at least this many implementation lines were *added*
    /// without test evidence. 1 = any new impl line needs a test.
    pub min_added_impl_lines: usize,
    /// Strict test/impl author separation (opt-in, default off — backward
    /// compatible). Mirrors condukt's verifier-model≠worker-model invariant:
    /// when true, `tdd green` fail-closed rejects if the RED (test-authoring)
    /// identity equals the GREEN (implementation) identity, preventing a single
    /// agent from writing both a wrong implementation and a matching wrong test
    /// (reward hacking). See `proof::judge_separation`.
    pub strict_separation: bool,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    enabled: Option<bool>,
    max_attempts: Option<u32>,
    reset_after_secs: Option<i64>,
    state_dir: Option<String>,
    proof_dir: Option<String>,
    test_cmd: Option<String>,
    default_timeout_secs: Option<u64>,
    output_tail_lines: Option<usize>,
    impl_globs: Option<Vec<String>>,
    test_path_globs: Option<Vec<String>>,
    test_markers: Option<Vec<String>>,
    min_added_impl_lines: Option<usize>,
    strict_separation: Option<bool>,
}

/// The `~/.tdd` base directory. Thin wrapper over the shared primitive.
pub fn base_dir() -> PathBuf {
    harness_core::config::base_dir("tdd")
}

/// Emit a one-time, best-effort notice that an untrusted project `tdd.toml` was
/// ignored. Printed at most once per process so a hook that loads the config
/// repeatedly doesn't spam stderr.
fn warn_untrusted(path: &Path) {
    use std::sync::Once;
    static WARNED: Once = Once::new();
    WARNED.call_once(|| {
        eprintln!(
            "tdd: {} is not trusted; ignoring it. Run 'tdd trust' to enable.",
            path.display()
        );
    });
}

fn default_impl_globs() -> Vec<String> {
    [
        "**/*.rs",
        "**/*.py",
        "**/*.ts",
        "**/*.tsx",
        "**/*.js",
        "**/*.jsx",
        "**/*.go",
        "**/*.java",
        "**/*.rb",
        "**/*.c",
        "**/*.cc",
        "**/*.cpp",
        "**/*.h",
        "**/*.hpp",
        "**/*.kt",
        "**/*.swift",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn default_test_path_globs() -> Vec<String> {
    [
        "**/tests/**",
        "**/test/**",
        "**/__tests__/**",
        "**/*_test.*",
        "**/test_*.py",
        "**/*.test.*",
        "**/*.spec.*",
        "**/*_spec.rb",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn default_test_markers() -> Vec<String> {
    [
        r"#\[\s*(tokio::|async_std::|rstest|test)",
        r"\bfn\s+test_",
        r"\bdef\s+test_",
        r"\bfunc\s+Test\w",
        r"\b(it|test|describe)\s*\(",
        r"@Test\b",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            max_attempts: 3,
            reset_after_secs: 600,
            state_dir: base_dir().join("state"),
            proof_dir: ".tdd".to_string(),
            test_cmd: "cargo test".to_string(),
            default_timeout_secs: 300,
            output_tail_lines: 40,
            impl_globs: default_impl_globs(),
            test_path_globs: default_test_path_globs(),
            test_markers: default_test_markers(),
            min_added_impl_lines: 1,
            strict_separation: false,
        }
    }
}

impl Config {
    pub fn project_path(root: &Path) -> PathBuf {
        root.join("tdd.toml")
    }

    pub fn home_path() -> PathBuf {
        base_dir().join("config.toml")
    }

    /// Load config for a project root. A project `tdd.toml` wins outright — but
    /// only once the project root is **trusted** (`harness_core::trust`), because
    /// its `test_cmd` is later executed verbatim by `tdd red`/`tdd green` and a
    /// malicious repo could otherwise smuggle in an arbitrary command. An
    /// untrusted project `tdd.toml` is ignored (with a one-time notice) and we
    /// fall back to the trusted home config, otherwise built-in defaults. Any
    /// parse error silently falls back (the gate must never crash a turn).
    pub fn load(root: &Path) -> Self {
        let mut cfg = Config::default();

        let chosen = {
            let p = Config::project_path(root);
            if p.exists() && trust::is_trusted(root) {
                Some(p)
            } else {
                if p.exists() {
                    warn_untrusted(&p);
                }
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
                    if let Some(v) = fc.reset_after_secs {
                        cfg.reset_after_secs = v;
                    }
                    if let Some(v) = fc.state_dir {
                        cfg.state_dir = expand_tilde(&v);
                    }
                    if let Some(v) = fc.proof_dir {
                        cfg.proof_dir = v;
                    }
                    if let Some(v) = fc.test_cmd {
                        cfg.test_cmd = v;
                    }
                    if let Some(v) = fc.default_timeout_secs {
                        cfg.default_timeout_secs = v;
                    }
                    if let Some(v) = fc.output_tail_lines {
                        cfg.output_tail_lines = v;
                    }
                    if let Some(v) = fc.impl_globs {
                        cfg.impl_globs = v;
                    }
                    if let Some(v) = fc.test_path_globs {
                        cfg.test_path_globs = v;
                    }
                    if let Some(v) = fc.test_markers {
                        cfg.test_markers = v;
                    }
                    if let Some(v) = fc.min_added_impl_lines {
                        cfg.min_added_impl_lines = v;
                    }
                    if let Some(v) = fc.strict_separation {
                        cfg.strict_separation = v;
                    }
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
        if cfg.proof_dir.trim().is_empty() {
            cfg.proof_dir = ".tdd".to_string();
        }
        if cfg.test_cmd.trim().is_empty() {
            cfg.test_cmd = "cargo test".to_string();
        }
        cfg
    }

    /// Globally disabled via env.
    pub fn disabled_env() -> bool {
        std::env::var("TDD_DISABLE")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn unique_dir(tag: &str) -> PathBuf {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("tdd-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    // Mutates the process-global HOME and HARNESS_TRUST_ALL env, so the whole
    // trust matrix is exercised in a SINGLE #[test] to keep it serialized.
    #[test]
    fn project_test_cmd_is_gated_behind_workspace_trust() {
        let home = unique_dir("trust-home");
        let proj = unique_dir("trust-proj");
        std::env::set_var("HOME", &home);
        std::env::remove_var("HARNESS_TRUST_ALL");

        // A malicious project-local config that would run an attacker command.
        std::fs::write(Config::project_path(&proj), "test_cmd = \"pwned\"\n").unwrap();

        // Untrusted + no home config → project is ignored, built-in default used.
        assert_eq!(
            Config::load(&proj).test_cmd,
            "cargo test",
            "untrusted project tdd.toml must NOT be honored; fall back to default"
        );

        // With a (trusted) home config present, an untrusted project still falls
        // back to HOME rather than the project file.
        let home_cfg = Config::home_path();
        std::fs::create_dir_all(home_cfg.parent().unwrap()).unwrap();
        std::fs::write(&home_cfg, "test_cmd = \"home-cmd\"\n").unwrap();
        assert_eq!(
            Config::load(&proj).test_cmd,
            "home-cmd",
            "untrusted project must fall back to the trusted home config"
        );

        // HARNESS_TRUST_ALL=1 trusts everything → project file honored.
        std::env::set_var("HARNESS_TRUST_ALL", "1");
        assert_eq!(
            Config::load(&proj).test_cmd,
            "pwned",
            "HARNESS_TRUST_ALL must let the project test_cmd through"
        );
        std::env::remove_var("HARNESS_TRUST_ALL");

        // Back to default-deny, then explicit trust via the shared trust list.
        assert_eq!(Config::load(&proj).test_cmd, "home-cmd");
        trust::add(&proj).unwrap();
        assert_eq!(
            Config::load(&proj).test_cmd,
            "pwned",
            "an explicitly trusted project must honor its own test_cmd"
        );

        // cleanup (best-effort)
        let _ = trust::remove(&proj);
        std::env::remove_var("HOME");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&proj);
    }
}
