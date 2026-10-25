//! Config: project `budgetguard.toml`, else `~/.budgetguard/config.toml`.
//!
//! A limit of 0 means "that axis is off". But a gate with NO limit at all is
//! not "within budget" (user ruling 2026-10-19: 「設定上限をきめて、決められない
//! ならその値にする、具体的にどうするかは経験値があればそれにもとづいて決める」).
//! When no config file exists, or every `[session]`/`[daily]` warn/block limit
//! is 0, the limits are set for the operator ([`LimitsSource`]):
//!
//! * **derived** from experience — the ledger history (`ledger.json` under
//!   `state_dir`), excluding today, when it holds at least
//!   [`MIN_HISTORY_DAYS`] past days, with Tukey outlier removal: daily block
//!   = the fence, daily warn = p90 of the days within it ([`derive_limits`]).
//!   The session axis gets NO derived warn and NO derived block (user ruling
//!   2026-10-19: 「警告はださなくていい、結果がみたいだけ」); the session fence is
//!   computed and reported by `check`/`status` as information only;
//! * otherwise the built-in **default** limits ([`FALLBACK_LIMITS`]);
//! * a ledger that exists but cannot be read or parsed is **undetermined**
//!   (the gate blocks), never silently replaced by the defaults.
//!
//! The explicit opt-outs are `enabled = false` and `BUDGETGUARD_DISABLE=1`.

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
    /// Emit a warning (systemMessage, shown to the user) when spend crosses this. 0 = disabled.
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

/// Where the effective USD limits came from. Reported by `check` / `status`
/// so a downstream reader can tell an operator's limit from one set for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitsSource {
    /// At least one limit was set (> 0) in the config file.
    Config,
    /// No limit was set; derived from the ledger history ([`derive_limits`]).
    Derived,
    /// No limit was set and the history was too short; [`FALLBACK_LIMITS`].
    Default,
}

impl LimitsSource {
    pub fn as_str(self) -> &'static str {
        match self {
            LimitsSource::Config => "config",
            LimitsSource::Derived => "derived",
            LimitsSource::Default => "default",
        }
    }
}

/// Limits set for an operator who configured none: the daily warn/block, plus
/// the session Tukey fence, which is information only (never a warn or block).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    pub daily_warn_usd: f64,
    pub daily_block_usd: f64,
    pub session_fence_usd: f64,
}

/// Past days (excluding today) the ledger must hold before limits are derived
/// from it rather than taken from [`FALLBACK_LIMITS`].
pub const MIN_HISTORY_DAYS: usize = 7;

/// Limits used when none is configured and the ledger history is shorter than
/// [`MIN_HISTORY_DAYS`]. Computed on 2026-10-19 from the operator's own
/// ledger (71 past days 2026-06-24..2026-10-18, 418 session entries) with the
/// same method as [`derive_limits`]: daily Tukey fence 1870.9 → block 1871,
/// p90 of the days at or below it 1336.3 → warn 1336; session fence 306.4 →
/// 306 (61 of 418 entries exceed it). The ~$2500 day in that history was a
/// runaway loop (user ruling 2026-10-19) and is excluded by the fence. The
/// session fence is reported only: per-session spend is heavy-tailed, so a
/// derived session block would trap ordinary long sessions, and the user
/// wants no session warning (「警告はださなくていい、結果がみたいだけ」). A
/// session warn/block only comes from the config.
pub const FALLBACK_LIMITS: Limits = Limits {
    daily_warn_usd: 1336.0,
    daily_block_usd: 1871.0,
    session_fence_usd: 306.0,
};

/// Derive limits from the ledger history, excluding `today`, with Tukey
/// outlier removal (Q1/Q3 by linear interpolation, fence = Q3 + 1.5 × IQR).
///
/// * `Ok(Some(limits))` — at least [`MIN_HISTORY_DAYS`] past days:
///   daily block = the fence over per-day totals, daily warn = p90 of the
///   per-day totals at or below that fence; session fence (information only)
///   = the fence over every past `(day, session)` entry (each entry is that
///   session's cost on that day).
/// * `Ok(None)` — not enough experience: fewer past days, no session entry,
///   or a history whose derived daily limits would not be positive (all-zero
///   spend). A non-positive limit would switch its axis OFF, so it is never
///   derived; the caller uses [`FALLBACK_LIMITS`].
/// * `Err(why)` — a recorded cost is negative or not finite; no limit can be
///   derived from it and silently skipping it would understate history.
pub fn derive_limits(
    ledger: &harness_core::ledger::Ledger,
    today: &str,
) -> Result<Option<Limits>, String> {
    let mut day_totals = Vec::new();
    let mut entries = Vec::new();
    for (date, day) in &ledger.days {
        if date == today {
            continue;
        }
        let mut total = 0.0;
        for (sid, &usd) in &day.sessions {
            if !(usd.is_finite() && usd >= 0.0) {
                return Err(format!(
                    "ledger entry {date}/{sid} is not a finite non-negative cost ({usd})"
                ));
            }
            total += usd;
            entries.push(usd);
        }
        day_totals.push(total);
    }
    if day_totals.len() < MIN_HISTORY_DAYS || entries.is_empty() {
        return Ok(None);
    }
    day_totals.sort_by(f64::total_cmp);
    entries.sort_by(f64::total_cmp);
    let day_fence = tukey_fence(&day_totals);
    let inlier_days: Vec<f64> = day_totals
        .iter()
        .copied()
        .filter(|v| *v <= day_fence)
        .collect();
    if inlier_days.is_empty() {
        return Ok(None);
    }
    let limits = Limits {
        daily_warn_usd: quantile(&inlier_days, 0.9),
        daily_block_usd: day_fence,
        session_fence_usd: tukey_fence(&entries),
    };
    let positive = [limits.daily_warn_usd, limits.daily_block_usd]
        .iter()
        .all(|v| v.is_finite() && *v > 0.0);
    Ok(positive.then_some(limits))
}

/// Upper Tukey fence `Q3 + 1.5 × (Q3 − Q1)` of a sorted, non-empty slice.
fn tukey_fence(sorted: &[f64]) -> f64 {
    let q1 = quantile(sorted, 0.25);
    let q3 = quantile(sorted, 0.75);
    q3 + 1.5 * (q3 - q1)
}

/// Quantile `p` of a sorted, non-empty slice by linear interpolation between
/// closest ranks (numpy's default method).
fn quantile(sorted: &[f64], p: f64) -> f64 {
    let k = (sorted.len() - 1) as f64 * p;
    let lo = k.floor() as usize;
    let hi = (lo + 1).min(sorted.len() - 1);
    sorted[lo] + (sorted[hi] - sorted[lo]) * (k - lo as f64)
}

#[derive(Debug, Clone)]
pub struct Config {
    pub enabled: bool,
    /// Where the four USD limits below came from.
    pub limits_source: LimitsSource,
    /// The session Tukey fence when the limits were derived or defaulted;
    /// information only, never compared against spend.
    pub session_fence_usd: Option<f64>,
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
            limits_source: LimitsSource::Config,
            session_fence_usd: None,
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
    /// Load the config as a THREE-answer result, with today's local date.
    pub fn load_checked(root: &Path) -> Determination<Config> {
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        Self::load_checked_at(root, &today)
    }

    /// Load the config as a THREE-answer result.
    ///
    /// * `Known(cfg)` — the config file was read (or there is none) and the
    ///   effective limits are known. When no limit is set (no file, or every
    ///   USD limit 0) and the gate is enabled, the limits are derived from the
    ///   ledger history or fall back to [`FALLBACK_LIMITS`] (see this module's
    ///   header); `cfg.limits_source` says which.
    /// * `Undetermined` — a config file EXISTS but could not be read or
    ///   parsed (the operator configured *something*; we cannot tell what),
    ///   or no limit is set and the ledger to derive one from exists but
    ///   cannot be read or parsed.
    ///
    /// Collapsing the second into the first is not a cosmetic difference: the
    /// caller receives a `Config` whose limits are all 0.0, and `gate::verdict`
    /// maps 0.0 to "no limit — allow any cost". Measured on this code before
    /// the split, a `budgetguard.toml` declaring `block_usd = 1.0` plus one
    /// syntax error produced `session_block_usd=0, daily_block_usd=0` and
    /// `verdict(999.0, 999.0) == Allow`. A typo silently disarmed the gate.
    /// `an_unparseable_config_is_undetermined_not_an_unconfigured_gate` pins
    /// it.
    pub fn load_checked_at(root: &Path, today: &str) -> Determination<Config> {
        let cfg = match Self::locate(root) {
            Determination::Known(Some(p)) => match Self::read_file(&p) {
                Determination::Known(cfg) => cfg,
                Determination::Undetermined(why) => return Determination::Undetermined(why),
            },
            // No config file: start from the all-zero default, whose limits
            // are resolved below like any other "no limit set" config.
            Determination::Known(None) => Config::default(),
            Determination::Undetermined(why) => return Determination::Undetermined(why),
        };
        Self::resolve_unset_limits(cfg, today)
    }

    /// No USD limit is set (every one `<= 0`, or not a number): set them from
    /// experience, else the built-in defaults. A disabled gate is left alone.
    fn resolve_unset_limits(mut cfg: Config, today: &str) -> Determination<Config> {
        let any_set = [
            cfg.session_warn_usd,
            cfg.session_block_usd,
            cfg.daily_warn_usd,
            cfg.daily_block_usd,
        ]
        .iter()
        .any(|v| *v > 0.0);
        if !cfg.enabled || any_set {
            return Determination::Known(cfg);
        }
        let ledger = match harness_core::ledger::Ledger::load_checked(&cfg.state_dir) {
            Ok(l) => l,
            Err(e) => {
                return Determination::undetermined(format!(
                    "no budget limit is configured, and the spend history in {} to derive one \
                     from could not be used: {e}",
                    cfg.state_dir.display()
                ))
            }
        };
        let (limits, source) = match derive_limits(&ledger, today) {
            Ok(Some(l)) => (l, LimitsSource::Derived),
            Ok(None) => (FALLBACK_LIMITS, LimitsSource::Default),
            Err(why) => {
                return Determination::undetermined(format!(
                    "no budget limit is configured, and the spend history in {} to derive one \
                     from is invalid: {why}",
                    cfg.state_dir.display()
                ))
            }
        };
        // The session axis stays off (warn and block 0): only the fence is kept.
        cfg.session_fence_usd = Some(limits.session_fence_usd);
        cfg.daily_warn_usd = limits.daily_warn_usd;
        cfg.daily_block_usd = limits.daily_block_usd;
        cfg.limits_source = source;
        Determination::Known(cfg)
    }

    /// Read and parse one config file over the defaults.
    fn read_file(path: &Path) -> Determination<Config> {
        let mut cfg = Config::default();
        let path = path.to_path_buf();

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
        // A [[price]] row that cannot be applied as written makes the whole
        // config undetermined, like a parse error: an empty pattern matches
        // EVERY model (so `pattern = "" input = 0.0` would price all spend at
        // $0), and a NaN / infinite / negative rate yields a cost no limit can
        // be compared against (backlog 00880b3d).
        let mut overrides = Vec::with_capacity(fc.price.len());
        for (i, p) in fc.price.into_iter().enumerate() {
            let n = i + 1;
            if p.pattern.trim().is_empty() {
                return Determination::undetermined(format!(
                    "budgetguard config {}: [[price]] row {n} has an empty `pattern`, which \
                     would match every model",
                    path.display()
                ));
            }
            if !(p.input.is_finite() && p.input >= 0.0 && p.output.is_finite() && p.output >= 0.0) {
                return Determination::undetermined(format!(
                    "budgetguard config {}: [[price]] row {n} (pattern {:?}) has a rate that \
                     is not a finite non-negative number (input = {}, output = {})",
                    path.display(),
                    p.pattern,
                    p.input,
                    p.output
                ));
            }
            overrides.push(PriceOverride {
                pattern: p.pattern,
                input: p.input,
                output: p.output,
            });
        }
        cfg.price_overrides = overrides;
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

    /// CONTRACT CHANGE (user ruling 2026-10-19, 「設定上限をきめて、決められない
    /// ならその値にする」): this test used to pin "no config file = every limit
    /// 0 = no-op". No config file is still a KNOWN answer (not undetermined),
    /// but with no ledger history the built-in default limits apply.
    #[test]
    fn an_absent_config_with_no_history_gets_the_default_limits() {
        with_scratch_home(|root| match Config::load_checked(root) {
            Determination::Known(cfg) => {
                assert_eq!(cfg.limits_source, LimitsSource::Default);
                assert_eq!(cfg.daily_block_usd, FALLBACK_LIMITS.daily_block_usd);
                assert_eq!(cfg.daily_warn_usd, FALLBACK_LIMITS.daily_warn_usd);
                // Session axis: no warn, no block; the fence is information.
                assert_eq!(cfg.session_warn_usd, 0.0);
                assert_eq!(cfg.session_block_usd, 0.0);
                assert_eq!(
                    cfg.session_fence_usd,
                    Some(FALLBACK_LIMITS.session_fence_usd)
                );
                assert!(cfg.enabled);
            }
            Determination::Undetermined(why) => {
                unreachable!("an absent config is not undetermined: {why}")
            }
        });
    }

    fn ledger_of(days: &[(&str, &[f64])]) -> harness_core::ledger::Ledger {
        let mut l = harness_core::ledger::Ledger::default();
        for (date, sessions) in days {
            for (i, usd) in sessions.iter().enumerate() {
                l.record(&format!("s{i}"), date, *usd);
            }
        }
        l
    }

    /// Seven past days of 10/20/.../70 with one session each, plus an outlier
    /// day of 1000 and a TODAY entry that must be ignored. Day totals sorted:
    /// 10..70, 1000 (n=8). Q1 = 27.5, Q3 = 62.5, fence = 115. Inliers 10..70:
    /// p90 = 64. Sessions are the same values, so the session fence is 115.
    #[test]
    fn derive_limits_uses_the_tukey_fence_and_ignores_today() {
        let l = ledger_of(&[
            ("2026-01-01", &[10.0]),
            ("2026-01-02", &[20.0]),
            ("2026-01-03", &[30.0]),
            ("2026-01-04", &[40.0]),
            ("2026-01-05", &[50.0]),
            ("2026-01-06", &[60.0]),
            ("2026-01-07", &[70.0]),
            ("2026-01-08", &[1000.0]),
            ("2026-01-09", &[99999.0]),
        ]);
        let got = derive_limits(&l, "2026-01-09").unwrap().unwrap();
        assert!((got.daily_block_usd - 115.0).abs() < 1e-9, "{got:?}");
        assert!((got.daily_warn_usd - 64.0).abs() < 1e-9, "{got:?}");
        assert!((got.session_fence_usd - 115.0).abs() < 1e-9, "{got:?}");
    }

    #[test]
    fn derive_limits_needs_seven_past_days() {
        let six: Vec<(String, f64)> = (1..=6).map(|d| (format!("2026-01-0{d}"), 10.0)).collect();
        let days: Vec<(&str, &[f64])> = six
            .iter()
            .map(|(d, v)| (d.as_str(), std::slice::from_ref(v)))
            .collect();
        // Six past days plus today is still six past days.
        let mut with_today = days.clone();
        with_today.push(("2026-01-09", &[10.0]));
        assert_eq!(
            derive_limits(&ledger_of(&with_today), "2026-01-09").unwrap(),
            None
        );
    }

    #[test]
    fn derive_limits_never_derives_a_zero_limit() {
        let days: Vec<(String, f64)> = (1..=8).map(|d| (format!("2026-01-0{d}"), 0.0)).collect();
        let days: Vec<(&str, &[f64])> = days
            .iter()
            .map(|(d, v)| (d.as_str(), std::slice::from_ref(v)))
            .collect();
        assert_eq!(
            derive_limits(&ledger_of(&days), "2026-02-01").unwrap(),
            None
        );
    }

    #[test]
    fn derive_limits_rejects_a_non_finite_cost() {
        let l = ledger_of(&[("2026-01-01", &[f64::NAN])]);
        assert!(derive_limits(&l, "2026-02-01").is_err());
    }

    /// With seven past days in the ledger and no config, the limits come from
    /// the ledger (source `derived`), not the defaults.
    #[test]
    fn an_absent_config_with_history_derives_its_limits() {
        with_scratch_home(|root| {
            let state = base_dir().join("state");
            let days: Vec<(String, f64)> = (1..=7)
                .map(|d| (format!("2026-01-0{d}"), d as f64 * 10.0))
                .collect();
            let days: Vec<(&str, &[f64])> = days
                .iter()
                .map(|(d, v)| (d.as_str(), std::slice::from_ref(v)))
                .collect();
            ledger_of(&days).save(&state).unwrap();
            match Config::load_checked_at(root, "2026-02-01") {
                Determination::Known(cfg) => {
                    assert_eq!(cfg.limits_source, LimitsSource::Derived);
                    assert!(cfg.daily_block_usd > 0.0 && cfg.daily_warn_usd > 0.0);
                    assert_ne!(cfg.daily_block_usd, FALLBACK_LIMITS.daily_block_usd);
                }
                Determination::Undetermined(why) => unreachable!("{why}"),
            }
        });
    }

    /// No limit configured and a corrupt ledger: the history cannot be read,
    /// so this is undetermined — never silently the defaults.
    #[test]
    fn an_absent_config_with_a_corrupt_ledger_is_undetermined() {
        with_scratch_home(|root| {
            let state = base_dir().join("state");
            std::fs::create_dir_all(&state).unwrap();
            std::fs::write(state.join("ledger.json"), "{ not json").unwrap();
            assert!(matches!(
                Config::load_checked(root),
                Determination::Undetermined(_)
            ));
        });
    }

    /// One axis set, the rest 0: a configured gate; the zero axes stay off and
    /// nothing is derived.
    #[test]
    fn one_set_limit_keeps_the_others_off() {
        with_scratch_home(|root| {
            std::fs::write(root.join("budgetguard.toml"), "[daily]\nblock_usd = 5.0\n").unwrap();
            match Config::load_checked(root) {
                Determination::Known(cfg) => {
                    assert_eq!(cfg.limits_source, LimitsSource::Config);
                    assert_eq!(cfg.daily_block_usd, 5.0);
                    assert_eq!(cfg.session_warn_usd, 0.0);
                    assert_eq!(cfg.session_block_usd, 0.0);
                    assert_eq!(cfg.daily_warn_usd, 0.0);
                }
                Determination::Undetermined(why) => unreachable!("{why}"),
            }
        });
    }

    /// `enabled = false` is the explicit opt-out: nothing is derived.
    #[test]
    fn a_disabled_config_derives_nothing() {
        with_scratch_home(|root| {
            std::fs::write(root.join("budgetguard.toml"), "enabled = false\n").unwrap();
            match Config::load_checked(root) {
                Determination::Known(cfg) => {
                    assert!(!cfg.enabled);
                    assert_eq!(cfg.limits_source, LimitsSource::Config);
                    assert_eq!(cfg.daily_block_usd, 0.0);
                }
                Determination::Undetermined(why) => unreachable!("{why}"),
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
