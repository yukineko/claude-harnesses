//! Configuration: project `propguard.toml` (preferred, once the project root is
//! trusted) layered over a home-level `~/.propguard/config.toml`, over built-in
//! defaults.
//!
//! Safe by default: with no config propguard checks ordinary source changes, but
//! if it can't find a task's `done_criteria` (nothing to derive properties
//! from), or nothing checkable changed, or there is no git repo, it lets every
//! stop through. Installing the hook can never *trap* a turn on its own.

use std::path::{Path, PathBuf};

use serde::Deserialize;

// Re-exported so `crate::config::expand_tilde` call sites keep working.
pub use harness_core::config::expand_tilde;

/// How the property check is actually performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Block the stop and inject the derived property checklist; the running
    /// (subscription) agent self-verifies its own code against each property.
    /// No API key, no extra process.
    Inject,
    /// Spawn `checker_cmd` as an independent checker over the properties + diff
    /// and count how many properties it reports satisfied; block only when
    /// fewer than `threshold` hold.
    Subprocess,
}

impl Mode {
    fn parse(s: &str) -> Mode {
        match s.trim().to_ascii_lowercase().as_str() {
            "subprocess" | "checker" | "independent" => Mode::Subprocess,
            _ => Mode::Inject,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Inject => "inject",
            Mode::Subprocess => "subprocess",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub enabled: bool,
    pub mode: Mode,
    /// Minimum number of semantic properties to derive. Derivation pads with
    /// baseline (universal) invariants when the done_criteria yields fewer.
    pub min_properties: usize,
    /// Cap on the number of properties derived (the task asks for 3–5).
    pub max_properties: usize,
    /// Block the stop when fewer than this many derived properties are
    /// satisfied. Clamped to the number of derived properties so a too-high
    /// threshold can never be permanently unsatisfiable.
    pub threshold: usize,
    /// After this many consecutive check rounds in one session, give up and
    /// allow the stop so the agent isn't trapped.
    pub max_attempts: u32,
    /// The per-session counter resets if this many seconds pass between stops.
    pub reset_after_secs: i64,
    /// Don't bother checking fewer than this many changed (generated) files.
    pub min_changed_files: usize,
    /// Cap the diff fed to the hasher / checker (bytes).
    pub max_diff_bytes: usize,
    /// Globs of generated files worth checking. A changed file must match one.
    pub include: Vec<String>,
    /// Globs never checked (lockfiles, vendored, generated…), applied after include.
    pub exclude: Vec<String>,
    /// Inline done_criteria (lowest-priority source; env + criteria_file win).
    pub done_criteria: String,
    /// Path (relative to the project root) of a file holding the current task's
    /// done_criteria. condukt / the agent can write it there.
    pub criteria_file: String,
    /// subprocess mode: command line that receives the check prompt on stdin and
    /// prints one `PROP <id>: PASS|FAIL` line per property on stdout.
    pub checker_cmd: String,
    pub checker_timeout_secs: u64,
    pub state_dir: PathBuf,
}

/// On-disk form; every field optional.
#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    enabled: Option<bool>,
    mode: Option<String>,
    min_properties: Option<usize>,
    max_properties: Option<usize>,
    threshold: Option<usize>,
    max_attempts: Option<u32>,
    reset_after_secs: Option<i64>,
    min_changed_files: Option<usize>,
    max_diff_bytes: Option<usize>,
    include: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
    done_criteria: Option<String>,
    criteria_file: Option<String>,
    checker_cmd: Option<String>,
    checker_timeout_secs: Option<u64>,
    state_dir: Option<String>,
}

/// The `~/.propguard` base directory. Thin wrapper over the shared helper.
pub fn base_dir() -> PathBuf {
    harness_core::config::base_dir("propguard")
}

/// Default filename propguard looks for a task's done_criteria in.
pub const DEFAULT_CRITERIA_FILE: &str = ".propguard-criteria";

fn default_include() -> Vec<String> {
    [
        "**/*.rs",
        "**/*.ts",
        "**/*.tsx",
        "**/*.js",
        "**/*.jsx",
        "**/*.py",
        "**/*.go",
        "**/*.java",
        "**/*.kt",
        "**/*.rb",
        "**/*.php",
        "**/*.c",
        "**/*.h",
        "**/*.cc",
        "**/*.cpp",
        "**/*.hpp",
        "**/*.cs",
        "**/*.swift",
        "**/*.scala",
        "**/*.sh",
        "**/*.sql",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn default_exclude() -> Vec<String> {
    [
        "**/*.lock",
        "**/*.min.js",
        "**/*.min.css",
        "**/Cargo.lock",
        "**/package-lock.json",
        "**/pnpm-lock.yaml",
        "**/yarn.lock",
        "**/node_modules/**",
        "**/target/**",
        "**/dist/**",
        "**/build/**",
        "**/vendor/**",
        "**/.venv/**",
        "**/*_pb2.py",
        "**/*.generated.*",
        "**/*.snap",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            mode: Mode::Inject,
            min_properties: 3,
            max_properties: 5,
            threshold: 3,
            max_attempts: 2,
            reset_after_secs: 600,
            min_changed_files: 1,
            max_diff_bytes: 200_000,
            include: default_include(),
            exclude: default_exclude(),
            done_criteria: String::new(),
            criteria_file: DEFAULT_CRITERIA_FILE.to_string(),
            checker_cmd: "claude -p".to_string(),
            checker_timeout_secs: 300,
            state_dir: base_dir().join("state"),
        }
    }
}

impl Config {
    pub fn project_path(root: &Path) -> PathBuf {
        root.join("propguard.toml")
    }

    pub fn home_path() -> PathBuf {
        base_dir().join("config.toml")
    }

    /// Load config for a project root. A project `propguard.toml` wins outright —
    /// but only once the project root is **trusted** (`harness_core::trust`),
    /// since its `checker_cmd` is later run as a subprocess from the Stop hook
    /// and an untrusted, repo-shipped value would be arbitrary code execution.
    /// When the project file exists but the root is not trusted we ignore it and
    /// fall back to the (trusted) home config, then built-in defaults. Any parse
    /// error silently falls back (the gate must never crash a turn).
    pub fn load(root: &Path) -> Self {
        let mut cfg = Config::default();

        let chosen = {
            let p = Config::project_path(root);
            if p.exists() && harness_core::trust::is_trusted(root) {
                Some(p)
            } else {
                if p.exists() {
                    eprintln!(
                        "propguard: {} is not trusted; ignoring it. \
                         Run 'propguard trust' to enable.",
                        p.display()
                    );
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
                    cfg.apply(fc);
                }
            }
        }

        cfg.sanitize();
        cfg
    }

    fn apply(&mut self, fc: FileConfig) {
        if let Some(v) = fc.enabled {
            self.enabled = v;
        }
        if let Some(v) = fc.mode {
            self.mode = Mode::parse(&v);
        }
        if let Some(v) = fc.min_properties {
            self.min_properties = v;
        }
        if let Some(v) = fc.max_properties {
            self.max_properties = v;
        }
        if let Some(v) = fc.threshold {
            self.threshold = v;
        }
        if let Some(v) = fc.max_attempts {
            self.max_attempts = v;
        }
        if let Some(v) = fc.reset_after_secs {
            self.reset_after_secs = v;
        }
        if let Some(v) = fc.min_changed_files {
            self.min_changed_files = v;
        }
        if let Some(v) = fc.max_diff_bytes {
            self.max_diff_bytes = v;
        }
        if let Some(v) = fc.include {
            self.include = v;
        }
        if let Some(v) = fc.exclude {
            self.exclude = v;
        }
        if let Some(v) = fc.done_criteria {
            self.done_criteria = v;
        }
        if let Some(v) = fc.criteria_file {
            if !v.trim().is_empty() {
                self.criteria_file = v;
            }
        }
        if let Some(v) = fc.checker_cmd {
            self.checker_cmd = v;
        }
        if let Some(v) = fc.checker_timeout_secs {
            self.checker_timeout_secs = v;
        }
        if let Some(v) = fc.state_dir {
            self.state_dir = expand_tilde(&v);
        }
    }

    /// Clamp every knob into a sane, non-trapping range. In particular the block
    /// threshold is bounded below by 1 (a threshold of 0 would never block, so
    /// the gate would be a no-op) and — at check time, once properties are
    /// derived — above by the property count, so a too-high threshold can never
    /// be permanently unsatisfiable.
    fn sanitize(&mut self) {
        if self.max_properties == 0 {
            self.max_properties = 5;
        }
        if self.max_properties > 5 {
            // The task formalizes 3–5 properties; keep the cap honest.
            self.max_properties = 5;
        }
        if self.min_properties == 0 {
            self.min_properties = 1;
        }
        if self.min_properties > self.max_properties {
            self.min_properties = self.max_properties;
        }
        if self.threshold == 0 {
            self.threshold = 1;
        }
        if self.threshold > self.max_properties {
            self.threshold = self.max_properties;
        }
        if self.max_attempts == 0 {
            self.max_attempts = 1;
        }
        if self.reset_after_secs <= 0 {
            // A zero/negative reset window disables the idle-gap give-up
            // reset, so a block loop could never be escaped that way. Floor
            // it to the built-in default (CA-propguard-02).
            self.reset_after_secs = Config::default().reset_after_secs;
        }
        if self.min_changed_files == 0 {
            self.min_changed_files = 1;
        }
        if self.max_diff_bytes == 0 {
            self.max_diff_bytes = 200_000;
        }
        if self.checker_timeout_secs == 0 {
            self.checker_timeout_secs = 300;
        }
    }

    /// Globally disabled via env.
    pub fn disabled_env() -> bool {
        std::env::var("PROPGUARD_DISABLE")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_is_clamped_to_property_cap() {
        let mut cfg = Config {
            threshold: 99,
            max_properties: 5,
            ..Config::default()
        };
        cfg.sanitize();
        assert_eq!(cfg.threshold, 5, "threshold must not exceed max_properties");
    }

    #[test]
    fn zero_threshold_becomes_one_so_gate_is_never_a_noop() {
        let mut cfg = Config {
            threshold: 0,
            ..Config::default()
        };
        cfg.sanitize();
        assert_eq!(cfg.threshold, 1);
    }

    #[test]
    fn max_properties_capped_at_five() {
        let mut cfg = Config {
            max_properties: 42,
            ..Config::default()
        };
        cfg.sanitize();
        assert_eq!(cfg.max_properties, 5, "the task formalizes 3–5 properties");
    }

    #[test]
    fn min_never_exceeds_max() {
        let mut cfg = Config {
            min_properties: 10,
            max_properties: 4,
            ..Config::default()
        };
        cfg.sanitize();
        assert!(cfg.min_properties <= cfg.max_properties);
    }

    // ── CA-propguard-02: a non-positive reset_after_secs must not survive
    //    sanitize — it disables the give-up/reset escape hatch, so a block
    //    loop could never be escaped by an idle gap. ─────────────────────────
    #[test]
    fn zero_reset_after_secs_falls_back_to_default() {
        let mut cfg = Config {
            reset_after_secs: 0,
            ..Config::default()
        };
        cfg.sanitize();
        assert_eq!(
            cfg.reset_after_secs,
            Config::default().reset_after_secs,
            "a zero reset_after_secs must be floored to a sane default, not left disabling the reset"
        );
    }

    #[test]
    fn negative_reset_after_secs_falls_back_to_default() {
        let mut cfg = Config {
            reset_after_secs: -1,
            ..Config::default()
        };
        cfg.sanitize();
        assert_eq!(
            cfg.reset_after_secs,
            Config::default().reset_after_secs,
            "a negative reset_after_secs must be floored to a sane default"
        );
    }

    // ── checker_timeout_secs: default + user override propagation ──────────
    //
    // The checker subprocess timeout (README-documented) defaults to 300s and
    // must be overridable via the `checker_timeout_secs` key in the resolved
    // `propguard.toml` / `~/.propguard/config.toml`. These tests pin the
    // default and confirm `Config::apply` (the function `Config::load` calls
    // after parsing the on-disk `FileConfig`) actually propagates a
    // user-supplied value onto `Config.checker_timeout_secs`, which is what
    // `gate::run_checker` reads to bound the subprocess kill-on-timeout.
    #[test]
    fn checker_timeout_secs_defaults_to_300() {
        assert_eq!(
            Config::default().checker_timeout_secs,
            300,
            "README documents the default checker subprocess timeout as 300s"
        );
    }

    #[test]
    fn checker_timeout_secs_is_overridable_from_file_config() {
        let mut cfg = Config::default();
        let fc = FileConfig {
            checker_timeout_secs: Some(45),
            ..Default::default()
        };
        cfg.apply(fc);
        assert_eq!(
            cfg.checker_timeout_secs, 45,
            "a user-supplied checker_timeout_secs must override the 300s default"
        );
    }

    #[test]
    fn checker_timeout_secs_survives_sanitize_when_nonzero() {
        let mut cfg = Config {
            checker_timeout_secs: 45,
            ..Config::default()
        };
        cfg.sanitize();
        assert_eq!(
            cfg.checker_timeout_secs, 45,
            "sanitize must not clobber a valid user-supplied timeout"
        );
    }

    #[test]
    fn checker_timeout_secs_load_from_toml_text_overrides_default() {
        // Exercises the same `toml::from_str::<FileConfig>` + `apply` path that
        // `Config::load` uses, without touching the filesystem/trust gate.
        let text = "checker_timeout_secs = 45\n";
        let fc: FileConfig = toml::from_str(text).expect("valid toml");
        let mut cfg = Config::default();
        cfg.apply(fc);
        cfg.sanitize();
        assert_eq!(
            cfg.checker_timeout_secs, 45,
            "checker_timeout_secs in propguard.toml must reach Config.checker_timeout_secs"
        );
    }
}
