//! Config: project `budgetguard.toml` layered over `~/.budgetguard/config.toml`.
//!
//! All limits default to 0 (disabled). A limit of 0 means "no limit" — the gate
//! always allows the stop. This keeps the plugin safe-by-default: installing it
//! without configuring any limits is a no-op.

use std::path::{Path, PathBuf};

use serde::Deserialize;

pub use harness_core::config::expand_tilde;
pub use harness_core::pricing::PriceOverride;
use harness_core::verdict::Determination;

/// The project or home config file.
#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    enabled: Option<bool>,
    state_dir: Option<String>,
    #[serde(default)]
    session: BudgetLevel,
    #[serde(default)]
    daily: BudgetLevel,
    /// Override the built-in price table for specific models.
    #[serde(default)]
    price: Vec<PriceOverrideCfg>,
    #[serde(default)]
    cache: CacheLevel,
}

#[derive(Debug, Default, Deserialize)]
struct CacheLevel {
    /// Hit-rate floor (0.0–1.0). Out of range → the built-in default; a
    /// threshold cannot be used to switch the check off (CLAUDE.md §3).
    min_rate: Option<f64>,
    /// Input tokens required before the rate is judged at all.
    min_tokens: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct BudgetLevel {
    /// Emit a warning (additionalContext) when spend crosses this. 0 = disabled.
    warn_usd: Option<f64>,
    /// Block the stop when spend crosses this. 0 = disabled.
    block_usd: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct PriceOverrideCfg {
    pattern: String,
    input: f64,
    output: f64,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub enabled: bool,
    /// Per-session warn/block (USD). 0.0 = disabled.
    pub session_warn_usd: f64,
    pub session_block_usd: f64,
    /// Per-calendar-day warn/block (USD, all sessions). 0.0 = disabled.
    pub daily_warn_usd: f64,
    pub daily_block_usd: f64,
    pub state_dir: PathBuf,
    pub price_overrides: Vec<PriceOverride>,
    /// Hit-rate floor below which a session is reported degraded. Unlike the
    /// USD limits above, `0.0` does NOT mean "disabled" here: no rate is ever
    /// below zero, so honouring it would be a floorless clamp — the shape
    /// CLAUDE.md §3 names as "always pass". Out-of-range values fall back to
    /// [`crate::cache::DEFAULT_MIN_HIT_RATE`]; see `cache::effective_threshold`.
    pub cache_hit_min_rate: f64,
    /// `input + cache_read` tokens required before the rate is judged at all.
    pub cache_hit_min_tokens: u64,
}

pub fn base_dir() -> PathBuf {
    harness_core::config::base_dir("budgetguard")
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            session_warn_usd: 0.0,
            session_block_usd: 0.0,
            daily_warn_usd: 0.0,
            daily_block_usd: 0.0,
            state_dir: base_dir().join("state"),
            price_overrides: Vec::new(),
            cache_hit_min_rate: crate::cache::DEFAULT_MIN_HIT_RATE,
            cache_hit_min_tokens: crate::cache::DEFAULT_MIN_INPUT_TOKENS,
        }
    }
}

impl Config {
    /// Load the config as a THREE-answer result.
    ///
    /// The two answers that matter are easy to confuse and must not be:
    ///
    /// * `Known(default)` — no config file exists. The operator configured
    ///   nothing, every limit is 0.0, and the gate is genuinely disabled. This
    ///   is the documented safe-by-default install (see this module's header).
    /// * `Undetermined` — a config file EXISTS but could not be read or
    ///   parsed. The operator configured *something*; we cannot tell what.
    ///
    /// Collapsing the second into the first is not a cosmetic difference: the
    /// caller receives a `Config` whose limits are all 0.0, and `gate::verdict`
    /// maps 0.0 to "no limit — allow any cost". Measured on this code before
    /// the split, a `budgetguard.toml` declaring `block_usd = 1.0` plus one
    /// syntax error produced `session_block_usd=0, daily_block_usd=0` and
    /// `verdict(999.0, 999.0) == Allow`. A typo silently disarmed the gate.
    /// `an_unparseable_config_is_undetermined_not_an_unconfigured_gate` pins
    /// it; `an_absent_config_is_known_default_not_undetermined` pins the
    /// deliberate spec on the other side so the fix cannot overshoot into
    /// blocking every unconfigured install.
    pub fn load_checked(root: &Path) -> Determination<Config> {
        let mut cfg = Config::default();

        let path = match Self::locate(root) {
            Determination::Known(Some(p)) => p,
            // Genuinely unconfigured — the documented no-op install.
            Determination::Known(None) => return Determination::Known(cfg),
            Determination::Undetermined(why) => return Determination::Undetermined(why),
        };

        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                return Determination::undetermined(format!(
                    "budgetguard config {} exists but could not be read: {e}",
                    path.display()
                ))
            }
        };
        let fc = match toml::from_str::<FileConfig>(&text) {
            Ok(fc) => fc,
            Err(e) => {
                return Determination::undetermined(format!(
                    "budgetguard config {} could not be parsed: {e}",
                    path.display()
                ))
            }
        };

        if let Some(v) = fc.enabled {
            cfg.enabled = v;
        }
        if let Some(v) = fc.state_dir {
            cfg.state_dir = expand_tilde(&v);
        }
        if let Some(v) = fc.session.warn_usd {
            cfg.session_warn_usd = v;
        }
        if let Some(v) = fc.session.block_usd {
            cfg.session_block_usd = v;
        }
        if let Some(v) = fc.daily.warn_usd {
            cfg.daily_warn_usd = v;
        }
        if let Some(v) = fc.daily.block_usd {
            cfg.daily_block_usd = v;
        }
        cfg.price_overrides = fc
            .price
            .into_iter()
            .map(|p| PriceOverride {
                pattern: p.pattern,
                input: p.input,
                output: p.output,
            })
            .collect();
        // Carried through verbatim, NOT sanitized here: `cache::assess` clamps
        // at the point of use so that no caller can reach the comparison with an
        // unvalidated threshold. Sanitizing here as well would put the rule in
        // two places, and the copy that drifts is the one that stops clamping.
        if let Some(v) = fc.cache.min_rate {
            cfg.cache_hit_min_rate = v;
        }
        if let Some(v) = fc.cache.min_tokens {
            cfg.cache_hit_min_tokens = v;
        }

        Determination::Known(cfg)
    }

    /// Which config file applies: the project's, else the home fallback, else
    /// none. `Path::exists()` answers "no" both when a path is absent and when
    /// the question could not be asked (a permission error on an ancestor
    /// directory), so it is `try_exists` here — an unanswerable question is
    /// undetermined, not "unconfigured".
    fn locate(root: &Path) -> Determination<Option<PathBuf>> {
        let project = root.join("budgetguard.toml");
        match project.try_exists() {
            Ok(true) => return Determination::Known(Some(project)),
            Ok(false) => {}
            Err(e) => {
                return Determination::undetermined(format!(
                    "could not determine whether {} exists: {e}",
                    project.display()
                ))
            }
        }
        let home = base_dir().join("config.toml");
        match home.try_exists() {
            Ok(true) => Determination::Known(Some(home)),
            Ok(false) => Determination::Known(None),
            Err(e) => Determination::undetermined(format!(
                "could not determine whether {} exists: {e}",
                home.display()
            )),
        }
    }

    /// NOTE: `unwrap_or(false)` here is RESTRICTIVE, not permissive, and must
    /// stay that way. `false` means "not disabled" — an unset or malformed
    /// `BUDGETGUARD_DISABLE` leaves the gate ARMED. Reading this site by the
    /// shape of `unwrap_or` alone would misclassify it.
    pub fn disabled_env() -> bool {
        harness_core::config::env_bool("BUDGETGUARD_DISABLE").unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Point `$HOME` at a scratch dir so the `~/.budgetguard/config.toml`
    /// fallback cannot reach the developer's real config (which would make
    /// these assertions depend on the machine they run on).
    fn with_scratch_home<T>(f: impl FnOnce(&Path) -> T) -> T {
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        let root = tempfile::tempdir().unwrap();
        let out = f(root.path());
        std::env::remove_var("HOME");
        out
    }

    /// The operator armed a $1 block limit and typo'd the TOML. Silently
    /// falling back to `Config::default()` sets every limit to 0.0, and
    /// `verdict` reads 0.0 as "disabled → allow any cost" — so a parse failure
    /// would DISARM the gate rather than leave it as configured.
    #[test]
    fn an_unparseable_config_is_undetermined_not_an_unconfigured_gate() {
        with_scratch_home(|root| {
            std::fs::write(
                root.join("budgetguard.toml"),
                "[session]\nblock_usd = 1.0\nthis is not toml\n",
            )
            .unwrap();
            assert!(
                matches!(Config::load_checked(root), Determination::Undetermined(_)),
                "a config that could not be parsed must not be reported as a \
                 successfully-loaded (and therefore unconfigured) gate"
            );
        });
    }

    /// Same fault one layer down: the file is present but unreadable. A
    /// directory standing where the file should be makes `read_to_string`
    /// fail without depending on chmod semantics or the test running as root.
    #[test]
    fn an_unreadable_config_is_undetermined_not_an_unconfigured_gate() {
        with_scratch_home(|root| {
            std::fs::create_dir(root.join("budgetguard.toml")).unwrap();
            assert!(
                matches!(Config::load_checked(root), Determination::Undetermined(_)),
                "a config that could not be read must not be reported as a \
                 successfully-loaded gate"
            );
        });
    }

    /// CONTROL — the deliberate spec from this module's header: "installing it
    /// without configuring any limits is a no-op". No config file at all is a
    /// KNOWN answer (the operator configured nothing), NOT undetermined.
    /// Without this, the fix above would turn every unconfigured install into a
    /// blocking gate.
    #[test]
    fn an_absent_config_is_known_default_not_undetermined() {
        with_scratch_home(|root| match Config::load_checked(root) {
            Determination::Known(cfg) => {
                assert_eq!(cfg.session_block_usd, 0.0);
                assert_eq!(cfg.daily_block_usd, 0.0);
                assert!(cfg.enabled);
            }
            Determination::Undetermined(why) => {
                unreachable!("an absent config is a genuine default, not undetermined: {why}")
            }
        });
    }

    /// ANTI-VACUITY CONTROL — proves the two assertions above are about the
    /// fault and not about the fixture: the same harness, with a well-formed
    /// config, yields `Known` and carries the operator's limits through.
    #[test]
    fn a_valid_config_is_known_and_carries_its_limits() {
        with_scratch_home(|root| {
            std::fs::write(
                root.join("budgetguard.toml"),
                "[session]\nblock_usd = 1.5\n\n[daily]\nwarn_usd = 7.0\n",
            )
            .unwrap();
            match Config::load_checked(root) {
                Determination::Known(cfg) => {
                    assert_eq!(cfg.session_block_usd, 1.5);
                    assert_eq!(cfg.daily_warn_usd, 7.0);
                }
                Determination::Undetermined(why) => {
                    unreachable!("a well-formed config must load: {why}")
                }
            }
        });
    }

    /// The home fallback is the same read+parse pair, so it must resolve the
    /// same way — otherwise the hole simply moves one branch over.
    #[test]
    fn an_unparseable_home_config_is_also_undetermined() {
        with_scratch_home(|root| {
            let home_cfg = base_dir();
            std::fs::create_dir_all(&home_cfg).unwrap();
            std::fs::write(home_cfg.join("config.toml"), "not = = toml\n").unwrap();
            assert!(
                matches!(Config::load_checked(root), Determination::Undetermined(_)),
                "the ~/.budgetguard/config.toml fallback must fail closed too"
            );
        });
    }
}
