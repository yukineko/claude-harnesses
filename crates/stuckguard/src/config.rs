//! Configuration: project `stuckguard.toml` (preferred) over a home-level
//! `~/.stuckguard/config.toml` over built-in defaults. Env override last.
//!
//! Safe by default: detection only ever *injects advice*; it can never block a
//! tool call or end a turn. Worst case is a spurious nudge, which the cooldown
//! and thresholds are tuned to avoid.

use std::path::{Path, PathBuf};

use serde::Deserialize;

// Re-exported so existing `crate::config::expand_tilde` call sites keep working.
pub use harness_core::config::expand_tilde;

/// Floor for `similarity_threshold` after sanitization. `0.0` is a degenerate
/// value: `jaccard(..) >= 0.0` is trivially true for any two token bags, so
/// the near-repeat filter would match every same-tool call regardless of
/// actual overlap (nudge-on-every-action). A small positive floor keeps the
/// near-repeat comparison meaningful (something must actually overlap) while
/// still allowing operators to opt into an aggressive near-repeat match via a
/// low threshold. Sanitization only — never panics.
const MIN_SIMILARITY_THRESHOLD: f64 = 0.05;

#[derive(Debug, Clone)]
pub struct Config {
    pub enabled: bool,
    /// Rolling window of recent tool events kept per session and inspected.
    pub window: usize,
    /// Same normalized (tool, input) this many times in the window ⇒ nudge.
    pub repeat_threshold: usize,
    /// Jaccard token-bag similarity (in `[0, 1]`) above which two calls of
    /// the SAME tool count as a "near-repeat" even when their exact `sig`
    /// differs. `1.0` (the default) means only byte-identical signatures
    /// count — i.e. behavior is unchanged unless an operator opts in by
    /// lowering this below `1.0`. Deterministic pure token-set overlap, no
    /// RAG/embeddings.
    pub similarity_threshold: f64,
    /// Revert/thrash reversals on one file in the window ⇒ nudge.
    pub oscillation_threshold: usize,
    /// Don't re-nudge the same pattern within this many new events.
    pub cooldown_events: u64,
    /// After this many nudges for the same pattern, escalate to "ask the user".
    pub escalate_after: u32,
    /// Tools excluded from detection entirely (e.g. TodoWrite bookkeeping).
    pub ignore_tools: Vec<String>,
    pub state_dir: PathBuf,
    /// Enable the early, soft "progress may be stalling" advisory (a
    /// 3-signal `progress_score` computed over the recent window,
    /// independent of and lower-severity than the hard repeat/oscillation
    /// escalation below). `false` by default — the advisory is opt-in, so
    /// existing behavior is completely unchanged unless an operator turns it
    /// on in config.
    pub progress_advisory_enabled: bool,
    /// Minimum window length before the advisory is even considered (avoids
    /// judging "diversity"/"stability" on too few samples).
    pub progress_min_window: usize,
    /// `progress_score` (in `[0, 1]`, higher = more likely stalling) at or
    /// above which the advisory fires. Conservative (high) by default so a
    /// mildly repetitive-but-fine window doesn't trip it.
    pub progress_score_threshold: f64,
    /// Enable the PDO scope-drift advisory (§4.4): nudge when recent edits fall
    /// outside the session's declared anchor scope. `false` by default — opt-in
    /// so existing behavior is unchanged until an operator turns it on and the
    /// session actually holds an overwatch lease with a non-empty scope.
    pub scope_drift_enabled: bool,
    /// Consecutive out-of-scope edits before the scope-drift advisory fires.
    pub drift_threshold: usize,
    /// Piggyback a `condukt`/`overwatch` heartbeat on every PostToolUse (§4.6b),
    /// so a long single task doesn't let its claim/lease go stale and get
    /// reaped/stolen. `true` by default — this is a safety feature (prevents
    /// task theft); disable only to opt out. No-op when the session holds no
    /// lease or the binaries are absent (fail-soft).
    pub heartbeat_piggyback_enabled: bool,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    enabled: Option<bool>,
    window: Option<usize>,
    repeat_threshold: Option<usize>,
    similarity_threshold: Option<f64>,
    oscillation_threshold: Option<usize>,
    cooldown_events: Option<u64>,
    escalate_after: Option<u32>,
    ignore_tools: Option<Vec<String>>,
    state_dir: Option<String>,
    progress_advisory_enabled: Option<bool>,
    progress_min_window: Option<usize>,
    progress_score_threshold: Option<f64>,
    scope_drift_enabled: Option<bool>,
    drift_threshold: Option<usize>,
    heartbeat_piggyback_enabled: Option<bool>,
}

/// The `~/.stuckguard` base directory.
pub fn base_dir() -> PathBuf {
    harness_core::config::base_dir("stuckguard")
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            window: 12,
            repeat_threshold: 3,
            // 1.0 = only byte-identical sigs count as a repeat, i.e. current
            // (pre-similarity-detection) behavior, unchanged unless an
            // operator opts in via config.
            similarity_threshold: 1.0,
            oscillation_threshold: 2,
            cooldown_events: 6,
            escalate_after: 2,
            ignore_tools: vec!["TodoWrite".to_string()],
            state_dir: base_dir().join("state"),
            // Default-off: the advisory is a new, additional signal and must
            // not change existing behavior unless an operator opts in.
            progress_advisory_enabled: false,
            progress_min_window: 6,
            // Conservative: score must be quite high (near-certain stall
            // signature across all 3 signals) before the advisory fires.
            progress_score_threshold: 0.75,
            // Default-off: scope-drift is a new advisory, opt-in like the
            // progress advisory.
            scope_drift_enabled: false,
            drift_threshold: 3,
            // Default-on: heartbeat piggyback is a safety feature (anti-theft),
            // no-op unless the session holds a lease.
            heartbeat_piggyback_enabled: true,
        }
    }
}

impl Config {
    pub fn project_path(root: &Path) -> PathBuf {
        root.join("stuckguard.toml")
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
                    if let Some(v) = fc.window {
                        cfg.window = v;
                    }
                    if let Some(v) = fc.repeat_threshold {
                        cfg.repeat_threshold = v;
                    }
                    if let Some(v) = fc.similarity_threshold {
                        cfg.similarity_threshold = v;
                    }
                    if let Some(v) = fc.oscillation_threshold {
                        cfg.oscillation_threshold = v;
                    }
                    if let Some(v) = fc.cooldown_events {
                        cfg.cooldown_events = v;
                    }
                    if let Some(v) = fc.escalate_after {
                        cfg.escalate_after = v;
                    }
                    if let Some(v) = fc.ignore_tools {
                        cfg.ignore_tools = v;
                    }
                    if let Some(v) = fc.state_dir {
                        cfg.state_dir = expand_tilde(&v);
                    }
                    if let Some(v) = fc.progress_advisory_enabled {
                        cfg.progress_advisory_enabled = v;
                    }
                    if let Some(v) = fc.progress_min_window {
                        cfg.progress_min_window = v;
                    }
                    if let Some(v) = fc.progress_score_threshold {
                        cfg.progress_score_threshold = v;
                    }
                    if let Some(v) = fc.scope_drift_enabled {
                        cfg.scope_drift_enabled = v;
                    }
                    if let Some(v) = fc.drift_threshold {
                        cfg.drift_threshold = v;
                    }
                    if let Some(v) = fc.heartbeat_piggyback_enabled {
                        cfg.heartbeat_piggyback_enabled = v;
                    }
                }
            }
        }
        // sanitize
        cfg.window = cfg.window.max(2);
        cfg.repeat_threshold = cfg.repeat_threshold.max(2);
        cfg.similarity_threshold = cfg
            .similarity_threshold
            .clamp(MIN_SIMILARITY_THRESHOLD, 1.0);
        cfg.oscillation_threshold = cfg.oscillation_threshold.max(1);
        cfg.escalate_after = cfg.escalate_after.max(1);
        cfg.progress_min_window = cfg.progress_min_window.max(2);
        cfg.progress_score_threshold = cfg.progress_score_threshold.clamp(0.0, 1.0);
        // A drift_threshold larger than the window can never be reached (same
        // reasoning as repeat_threshold above), and 0 would fire on the first
        // event. Floor at 1 and clamp down to the window (fail-safe).
        cfg.drift_threshold = cfg.drift_threshold.max(1).min(cfg.window);
        // Cross-field invariant (CA-stuckguard-002): the detectors only ever see
        // the last `window` events (State::push caps the buffer at `window`), so
        // a `repeat_threshold` or `progress_min_window` LARGER than `window` can
        // never be reached — detection silently disables itself. The independent
        // `.max(2)` floors above do not catch this. Clamp both down to `window`
        // so a misconfig degrades to "trips once the window is full" (fail-safe:
        // over-detect) instead of "never trips" (fail-open: under-detect).
        cfg.repeat_threshold = cfg.repeat_threshold.min(cfg.window);
        cfg.progress_min_window = cfg.progress_min_window.min(cfg.window);
        cfg
    }

    pub fn disabled_env() -> bool {
        std::env::var("STUCKGUARD_DISABLE")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false)
    }

    pub fn is_ignored(&self, tool: &str) -> bool {
        self.ignore_tools.iter().any(|t| t == tool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CA-stuckguard-002: a `repeat_threshold` / `progress_min_window` larger
    /// than `window` can never be reached (the detectors only ever see the last
    /// `window` events), silently disabling detection. `load()` must clamp both
    /// down to `window`. Before the fix this is RED: `repeat_threshold = 10`
    /// with `window = 5` survives sanitize unchanged.
    #[test]
    fn cross_field_thresholds_clamped_to_window() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            Config::project_path(dir.path()),
            "window = 5\nrepeat_threshold = 10\nprogress_min_window = 20\n",
        )
        .unwrap();
        let cfg = Config::load(dir.path());
        assert_eq!(cfg.window, 5);
        assert_eq!(
            cfg.repeat_threshold, 5,
            "repeat_threshold must be clamped to window"
        );
        assert_eq!(
            cfg.progress_min_window, 5,
            "progress_min_window must be clamped to window"
        );
    }

    /// PDO anchor defaults (§4.4/§4.6b): scope-drift is opt-in (off),
    /// heartbeat piggyback is a safety default (on), drift_threshold = 3.
    /// A bare project config (no anchor keys) must preserve these.
    #[test]
    fn pdo_anchor_defaults_are_preserved() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(Config::project_path(dir.path()), "enabled = true\n").unwrap();
        let cfg = Config::load(dir.path());
        assert!(!cfg.scope_drift_enabled, "scope_drift is opt-in (off)");
        assert!(
            cfg.heartbeat_piggyback_enabled,
            "heartbeat piggyback defaults on"
        );
        assert_eq!(cfg.drift_threshold, 3);
    }

    /// drift_threshold must be floored to ≥1 and clamped to the window.
    #[test]
    fn drift_threshold_clamped_to_window_and_floored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            Config::project_path(dir.path()),
            "window = 5\ndrift_threshold = 99\n",
        )
        .unwrap();
        let cfg = Config::load(dir.path());
        assert_eq!(cfg.drift_threshold, 5, "clamped down to window");

        std::fs::write(Config::project_path(dir.path()), "drift_threshold = 0\n").unwrap();
        let cfg = Config::load(dir.path());
        assert!(cfg.drift_threshold >= 1, "floored to at least 1");
    }

    /// CA-stuckguard-03 (p2 — threshold clamp has no floor): `load()`
    /// sanitizes `similarity_threshold` with `.clamp(0.0, 1.0)`, so a config
    /// of `0.0` sails straight through. At `0.0`, `jaccard(..) >=
    /// similarity_threshold` is trivially true for ANY two token bags (jaccard
    /// is always in `[0, 1]`), so `is_repeat_of`'s near-repeat branch matches
    /// every same-tool call regardless of actual overlap — the near-repeat
    /// filter degenerates into "same tool" and nudges on every action. Before
    /// the fix this test fails (RED): a project config setting
    /// `similarity_threshold = 0.0` comes out of `load()` unchanged at
    /// `0.0`.
    #[test]
    fn similarity_threshold_floor_prevents_degenerate_zero() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            Config::project_path(dir.path()),
            "similarity_threshold = 0.0\n",
        )
        .unwrap();

        let cfg = Config::load(dir.path());

        assert!(
            cfg.similarity_threshold > 0.0,
            "similarity_threshold must be floored above 0.0 (0.0 makes jaccard >= \
             threshold trivially true for every same-tool call, i.e. nudge-on-every-\
             action); got {}",
            cfg.similarity_threshold
        );
    }
}
