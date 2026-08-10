//! Configuration: project `stuckguard.toml` (preferred) over a home-level
//! `~/.stuckguard/config.toml` over built-in defaults. Env override last.
//!
//! Safe by default: detection only ever *injects advice*; it can never block a
//! tool call or end a turn. Worst case is a spurious nudge, which the cooldown
//! and thresholds are tuned to avoid.

use std::path::{Path, PathBuf};

use harness_core::boundary;
use harness_core::verdict::{Required, Verdict};
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

/// Floor for `progress_score_threshold` after sanitization. `0.0` is a
/// degenerate value: the advisory gate fires when `advisory.score >=
/// progress_score_threshold` (see `main::maybe_progress_advisory` /
/// `detect::progress_score`), and `progress_score` is always in `[0, 1]`, so a
/// threshold of `0.0` makes `score >= 0.0` trivially true for every scored
/// window — the advisory fires on every action regardless of the actual stall
/// signal, i.e. the gate becomes a tautology that silently disables the
/// detector as a meaningful signal. A small positive floor keeps the advisory
/// discriminating (some real stall signal must be present) while still letting
/// operators opt into an aggressive, low threshold. Sanitization only — never
/// panics.
const MIN_PROGRESS_SCORE_THRESHOLD: f64 = 0.05;

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

/// Diagnostic text for a config file that exists but could not be read
/// (`Required::Blocked` out of `boundary::read_to_string`). Pure — so the
/// "what gets printed" question is unit-testable without capturing stderr.
/// Names the path and the verdict's reason so a reader can never mistake
/// "config load failed, defaults kicked in" for "config was read and simply
/// had no overrides".
fn describe_unreadable_config(path: &Path, verdict: &Verdict) -> String {
    let why = verdict
        .reason()
        .map(|r| r.as_str())
        .unwrap_or("unknown reason");
    format!(
        "stuckguard: could not read config {} ({}); falling back to defaults",
        path.display(),
        why
    )
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

    /// Load config, printing any "config file exists but could not be read"
    /// diagnostic to stderr. Thin wrapper over
    /// [`Config::load_with_diagnostics`]; see there for the RED/GREEN seam
    /// this split exists to create.
    pub fn load(root: &Path) -> Self {
        Config::load_with_diagnostics(root, &mut |msg| eprintln!("{msg}"))
    }

    /// The real implementation behind [`Config::load`], with the diagnostic
    /// sink taken as a parameter instead of hardcoded to `eprintln!`. This
    /// exists so a test can observe *whether the diagnostic fires* (not just
    /// that the helper that formats it produces good text, and not just that
    /// the fallback-to-defaults behavior is reachable — both of which stayed
    /// green even with the `eprintln!` deleted). `load()` is the only real
    /// caller; tests call this directly with a `Vec`-collecting sink.
    pub fn load_with_diagnostics(root: &Path, diag: &mut dyn FnMut(String)) -> Self {
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
            // `Required::Determined(None)` ("file vanished between exists()
            // and here") and `Required::Determined(Some(text))` (file read
            // fine) both fall through to normal handling below without any
            // diagnostic: a config file that legitimately does not exist (or
            // stopped existing) is not a judgment failure, it is an
            // observation. `Required::Blocked` ("file is there but could not
            // be read" — permission denied, IO error) is different: it is a
            // judgment failure, and stuckguard is a pure advisory hook (never
            // blocks), so it still degrades to defaults, but it must not do so
            // silently (CLAUDE.md §3) — emit a diagnostic naming the path and
            // the reason so "config is default" is never misread as "config
            // was read and had no overrides".
            let text = match boundary::read_to_string(&path).require() {
                Required::Determined(text) => text,
                Required::Blocked(verdict) => {
                    diag(describe_unreadable_config(&path, &verdict));
                    None
                }
            };
            if let Some(text) = text {
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
        // CA-stuckguard-05: the number of edit reversals is bounded by the
        // number of edits in the window minus one (each later edit counts at
        // most one reversal, the first can't), i.e. at most `window - 1`. An
        // `oscillation_threshold >= window` can therefore never be reached and
        // silently disables oscillation detection. Floor at 1 and clamp down to
        // `window - 1` (fail-safe over-detect), mirroring the sibling clamps.
        cfg.oscillation_threshold = cfg.oscillation_threshold.max(1).min(cfg.window - 1);
        cfg.escalate_after = cfg.escalate_after.max(1);
        cfg.progress_min_window = cfg.progress_min_window.max(2);
        cfg.progress_score_threshold = cfg
            .progress_score_threshold
            .clamp(MIN_PROGRESS_SCORE_THRESHOLD, 1.0);
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

    /// Proves `describe_unreadable_config` names both the path and the
    /// blocking reason, so the printed diagnostic can never be mistaken for
    /// "config was read and had no overrides" (the failure mode the removed
    /// `.require().ok().flatten()` had: it discarded the `Verdict` entirely).
    #[test]
    fn describe_unreadable_config_names_path_and_reason() {
        let path = Path::new("/some/stuckguard.toml");
        let verdict = Verdict::undetermined("permission denied (test)");
        let msg = describe_unreadable_config(path, &verdict);
        assert!(
            msg.contains("/some/stuckguard.toml"),
            "diagnostic must name the path: {msg}"
        );
        assert!(
            msg.contains("permission denied (test)"),
            "diagnostic must carry the verdict's reason: {msg}"
        );
    }

    /// Proves the `Required::Blocked` branch of `Config::load` is actually
    /// reachable and still resolves to defaults (stuckguard is a pure
    /// advisory hook, so degrading is correct) — it is only the SILENCE that
    /// was wrong, not the fallback. A directory in place of the config file
    /// makes `std::fs::read_to_string` fail with a non-`NotFound` error
    /// (`IsADirectory`/similar), which `boundary::read_to_string` maps to
    /// `Undetermined`, i.e. `Required::Blocked` — the same branch a real
    /// unreadable/permission-denied file would take. This test does not
    /// capture stderr (the diagnostic itself is covered by
    /// `describe_unreadable_config_names_path_and_reason` above); it proves
    /// only that `load()` does not panic and that defaults survive an
    /// unreadable config file.
    #[test]
    fn load_falls_back_to_defaults_when_config_is_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        // A directory at the expected config path: exists() is true, but
        // read_to_string on it is an error other than NotFound -> Undetermined.
        std::fs::create_dir(Config::project_path(dir.path())).unwrap();

        let cfg = Config::load(dir.path());

        let defaults = Config::default();
        assert_eq!(cfg.window, defaults.window, "unreadable config -> defaults");
        assert_eq!(
            cfg.repeat_threshold, defaults.repeat_threshold,
            "unreadable config -> defaults"
        );
    }

    /// Proves the diagnostic actually FIRES on the `Required::Blocked` path —
    /// not just that the formatting helper produces good text
    /// (`describe_unreadable_config_names_path_and_reason` above) and not
    /// just that defaults survive (`load_falls_back_to_defaults_when_config_is_unreadable`
    /// above), neither of which would notice the `eprintln!` being deleted.
    /// Calls `load_with_diagnostics` directly with a `Vec`-collecting sink so
    /// the emission itself is observable without capturing stderr. Before the
    /// diagnostic was wired to the `diag` parameter (it called `eprintln!`
    /// directly, ignoring `diag`) this test was RED: `diags` stayed empty.
    #[test]
    fn load_with_diagnostics_emits_one_message_for_unreadable_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(Config::project_path(dir.path())).unwrap();

        let mut diags = Vec::new();
        let _cfg = Config::load_with_diagnostics(dir.path(), &mut |m| diags.push(m));

        assert_eq!(
            diags.len(),
            1,
            "an unreadable config file must emit exactly one diagnostic; got {diags:?}"
        );
        let msg = &diags[0];
        assert!(
            msg.contains(&Config::project_path(dir.path()).display().to_string()),
            "diagnostic must name the config path: {msg}"
        );
    }

    /// Anti-vacuity control for the test above: a config that reads
    /// successfully (even an empty/no-op one) must emit ZERO diagnostics, so
    /// an implementation that unconditionally pushes a message (regardless of
    /// whether the file was actually unreadable) does not pass either test.
    #[test]
    fn load_with_diagnostics_is_silent_for_a_readable_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(Config::project_path(dir.path()), "enabled = true\n").unwrap();

        let mut diags = Vec::new();
        let _cfg = Config::load_with_diagnostics(dir.path(), &mut |m| diags.push(m));

        assert!(
            diags.is_empty(),
            "a readable config must not emit any diagnostic; got {diags:?}"
        );
    }

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

    /// CA-stuckguard-04 (advisory progress gate tautology): `load()` sanitized
    /// `progress_score_threshold` with `.clamp(0.0, 1.0)`, so a config of `0.0`
    /// passed straight through. The advisory fires when `advisory.score >=
    /// progress_score_threshold` (see `main::maybe_progress_advisory`) and
    /// `progress_score` is always in `[0, 1]`, so a threshold of `0.0` makes
    /// `score >= 0.0` trivially true for every scored window — the advisory
    /// fires on every action, i.e. the gate is a tautology that silently
    /// disables the detector as a meaningful signal. Before the fix this test
    /// fails (RED): a project config setting `progress_score_threshold = 0.0`
    /// comes out of `load()` unchanged at `0.0`.
    #[test]
    fn progress_score_threshold_floor_prevents_degenerate_zero() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            Config::project_path(dir.path()),
            "progress_score_threshold = 0.0\n",
        )
        .unwrap();

        let cfg = Config::load(dir.path());

        assert!(
            cfg.progress_score_threshold > 0.0,
            "progress_score_threshold must be floored above 0.0 (0.0 makes score \
             >= threshold trivially true for every scored window, i.e. the advisory \
             fires on every action and the gate is a tautology); got {}",
            cfg.progress_score_threshold
        );
        assert_eq!(
            cfg.progress_score_threshold, MIN_PROGRESS_SCORE_THRESHOLD,
            "a 0.0 misconfig must degrade to the positive minimum floor"
        );
    }

    /// CA-stuckguard-05: `oscillation_threshold` was floored with `.max(1)` but
    /// never clamped against `window`. The reversal count is bounded by
    /// `window - 1` (each later edit counts at most one reversal, the first can
    /// never be one), so a threshold `>= window` can never be reached and
    /// silently disables oscillation detection. `load()` must clamp it down to
    /// at most `window - 1`. Before the fix this is RED: `oscillation_threshold
    /// = 10` with `window = 5` survives sanitize unchanged at 10 (> window-1),
    /// and the behavioral back-and-forth window below never trips.
    #[test]
    fn oscillation_threshold_clamped_below_window() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            Config::project_path(dir.path()),
            "window = 5\noscillation_threshold = 10\n",
        )
        .unwrap();
        let cfg = Config::load(dir.path());
        assert_eq!(cfg.window, 5);
        assert!(
            cfg.oscillation_threshold < cfg.window,
            "oscillation_threshold must be clamped to at most window-1 so it can \
             still be reached (reversals are bounded by window-1); got {} with \
             window {}",
            cfg.oscillation_threshold,
            cfg.window
        );

        // Behavioral: with the clamp, a threshold configured >= window still
        // leaves oscillation detection able to fire once the window fills with
        // back-and-forth edits (A->B, B->A, ... = window-1 reversals).
        use crate::detect::detect;
        use crate::sig::build;
        let edit = |old: &str, new: &str| {
            build(
                "Edit",
                Some(&serde_json::json!({
                    "file_path": "f.rs",
                    "old_string": old,
                    "new_string": new,
                })),
                None,
                true,
            )
            .unwrap()
        };
        let window = vec![
            edit("A", "B"),
            edit("B", "A"),
            edit("A", "B"),
            edit("B", "A"),
            edit("A", "B"),
        ];
        let trip = detect(&window, &cfg);
        assert!(
            trip.is_some(),
            "oscillation must still be detectable after clamping the threshold \
             below window; effective threshold {}",
            cfg.oscillation_threshold
        );
    }
}
