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
    ///
    /// This is the **effective** value consumed by `proof::green`. It is
    /// resolved from an explicit request (config file / CLI flag) layered over a
    /// gate-crate-context default (see [`Config::resolve_strict_separation`] /
    /// [`effective_strict_separation`]).
    pub strict_separation: bool,
    /// Whether `strict_separation` was **explicitly** set by the loaded config
    /// file (`Some(v)`), or left unspecified (`None`). Kept separate from the
    /// effective `strict_separation` above so the default resolver can tell an
    /// intentional opt-out apart from "never mentioned" and only apply the
    /// gate-crate default-on in the latter case. See
    /// [`Config::resolve_strict_separation`].
    pub strict_separation_explicit: Option<bool>,
}

/// Fleet gate crates that must not be handled loosely: for these crates,
/// `strict_separation` (RED/GREEN author-diversity, fail-closed) defaults **on**
/// when otherwise unspecified — the same safe-by-default stance as the rollout
/// `--canary` requirement for these same gates. Kept as a plain constant array
/// so the context predicate stays a pure, unit-testable function.
///
/// Must equal `scripts/rollout-plugins.sh`'s canonical `GATE_CRATES` **exactly**.
/// `scripts/continuous-audit.sh`'s `DEFAULT_TARGETS` is a strict *superset*
/// (it also carries audit-only crates like `backlog` that gate nothing), so it
/// is deliberately not mirrored here.
///
/// `overwatch` is a member for the same reason rollout-plugins.sh includes it:
/// it is not itself a defense gate, but it computes the canary health-gate
/// decision and records confirmed audit findings, so a regression in it removes
/// the safety net protecting the other gates. That makes a self-authored
/// RED/GREEN pair in `crates/overwatch/**` exactly as unsafe as in a defense gate.
///
/// Enforced by `scripts/check-gate-crates-sync.py` (this file is a tracked source).
pub const GATE_CRATES: &[&str] = &[
    "blastguard",
    "propguard",
    "specguard",
    "stuckguard",
    "mutategate",
    "overwatch",
];

/// Pure predicate: is `path` inside one of the [`GATE_CRATES`] (i.e. does it
/// contain a `crates/<gate-crate>/…` segment)? Side-effect free (no cwd/env
/// reads) so it is deterministically unit-testable. Matches on path
/// *components* rather than a substring so a bare gate name without the
/// `crates/` parent does not false-positive.
pub fn is_gate_crate_context(path: &Path) -> bool {
    let comps: Vec<&str> = path
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    comps
        .windows(2)
        .any(|w| w[0] == "crates" && GATE_CRATES.contains(&w[1]))
}

/// Pure resolver for the effective `strict_separation` value, layering:
/// **explicit specification > gate-crate-context default-on > global
/// default-off**.
///
/// - `explicit = Some(v)`: an explicit config-file setting or CLI flag — always
///   honored verbatim (so a gate crate can still be opted *out* with
///   `--no-strict-separation`).
/// - `explicit = None`: unspecified — default to `gate_context` (on inside a
///   gate crate, off elsewhere — the latter preserves the pre-existing
///   backward-compatible default-off behaviour for ordinary crates).
pub fn effective_strict_separation(explicit: Option<bool>, gate_context: bool) -> bool {
    explicit.unwrap_or(gate_context)
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
            strict_separation_explicit: None,
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
                        cfg.strict_separation_explicit = Some(v);
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

    /// Resolve the **effective** `strict_separation` for a working `root`,
    /// layering (highest priority first): a CLI override (`cli_override`, e.g.
    /// `--no-strict-separation` → `Some(false)`) > the config-file explicit
    /// setting (`self.strict_separation_explicit`) > a gate-crate-context
    /// default-on > the global default-off. Pure given its inputs (delegates to
    /// [`is_gate_crate_context`] / [`effective_strict_separation`]).
    pub fn resolve_strict_separation(&self, root: &Path, cli_override: Option<bool>) -> bool {
        let explicit = cli_override.or(self.strict_separation_explicit);
        effective_strict_separation(explicit, is_gate_crate_context(root))
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

    // ── gate-crate context detection (t2) ───────────────────────────────────
    //
    // Pure, deterministic path predicate — no cwd/env dependence. GATE_CRATES
    // get strict_separation on by default (safe-by-default, mirrors the
    // rollout --canary requirement for the same defensive gates).

    #[test]
    fn is_gate_crate_context_detects_gate_paths() {
        // Any `crates/<gate-crate>/…` prefix in the path → gate context.
        assert!(is_gate_crate_context(Path::new(
            "/repo/crates/specguard/src/main.rs"
        )));
        assert!(is_gate_crate_context(Path::new("crates/blastguard")));
        assert!(is_gate_crate_context(Path::new("/x/crates/propguard/y")));
        assert!(is_gate_crate_context(Path::new(
            "/x/crates/stuckguard/src/lib.rs"
        )));
        assert!(is_gate_crate_context(Path::new("crates/mutategate/mod.rs")));
        // `overwatch` is a GATE crate too. Both Rust copies of GATE_CRATES had
        // silently lost it, leaving strict_separation default-OFF inside
        // crates/overwatch/** so one agent could author both RED and GREEN in
        // the crate implementing the Continuous-Audit loop. Asserted here so
        // reverting the one-line addition fails this crate's own tests rather
        // than only the cross-source Python checker.
        assert!(is_gate_crate_context(Path::new(
            "/repo/crates/overwatch/src/main.rs"
        )));
        assert!(is_gate_crate_context(Path::new("crates/overwatch")));
    }

    #[test]
    fn is_gate_crate_context_rejects_non_gate_paths() {
        // A non-gate crate (like tdd itself) is NOT a gate context.
        assert!(!is_gate_crate_context(Path::new(
            "/repo/crates/tdd/src/main.rs"
        )));
        assert!(!is_gate_crate_context(Path::new("/repo/crates/condukt")));
        assert!(!is_gate_crate_context(Path::new("/repo")));
        assert!(!is_gate_crate_context(Path::new("crates")));
        // "crates" substring absent from the crates/<gate> layout → false
        // (a bare gate name without the `crates/` parent must not match).
        assert!(!is_gate_crate_context(Path::new("/repo/specguard/src")));
    }

    #[test]
    fn effective_strict_separation_hierarchy() {
        // Explicit specification > gate-crate default-on > global default-off.
        // gate context + unspecified → ON (safe by default)
        assert!(effective_strict_separation(None, true));
        // gate context + explicit off (e.g. --no-strict-separation) → OFF
        assert!(!effective_strict_separation(Some(false), true));
        // gate context + explicit on → ON
        assert!(effective_strict_separation(Some(true), true));
        // non-gate + unspecified → OFF (backward compatible)
        assert!(!effective_strict_separation(None, false));
        // non-gate + explicit on → ON (opt-in still works)
        assert!(effective_strict_separation(Some(true), false));
        // non-gate + explicit off → OFF
        assert!(!effective_strict_separation(Some(false), false));
    }

    #[test]
    fn config_resolve_strict_separation_uses_gate_context() {
        // End-to-end resolution through the Config method: an unset config in a
        // gate-crate dir resolves ON; a CLI --no override forces OFF even there;
        // a non-gate dir stays OFF (unchanged default).
        let cfg = Config::default(); // strict_separation_explicit = None
        assert!(cfg.resolve_strict_separation(Path::new("/r/crates/specguard/src"), None));
        assert!(!cfg.resolve_strict_separation(Path::new("/r/crates/specguard/src"), Some(false)));
        assert!(!cfg.resolve_strict_separation(Path::new("/r/crates/tdd/src"), None));

        // A config file that explicitly set strict_separation wins over gate ctx.
        let cfg_off = Config {
            strict_separation_explicit: Some(false),
            ..Config::default()
        };
        assert!(!cfg_off.resolve_strict_separation(Path::new("/r/crates/specguard/src"), None));
        // …but a CLI override still beats the config-file explicit.
        assert!(cfg_off.resolve_strict_separation(Path::new("/r/crates/specguard/src"), Some(true)));
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
