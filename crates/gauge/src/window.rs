//! Manually-registered rate-limit window (length + last reset time).
//!
//! There is no API that exposes how much time remains in the Anthropic
//! account's rate-limit window — `HookInput` doesn't carry it, and
//! `harness_core`'s `ContextWindow` is conversation context usage, a
//! different thing entirely. So this can't be auto-detected: the user reads
//! their own `/usage` screen and registers the window length + last reset
//! time here. Combined with the session store's existing `first_ts`/`last_ts`
//! (a proxy for "how long this has been running"), that lets `gauge status`
//! show an *approximate* time-to-reset — approximate because a reset event
//! itself is never observed, only inferred from the registered window length.
//!
//! Unset (no file, or unparseable): every caller treats this as `None` and
//! falls back to showing continuous-uptime only (fail-soft — see
//! `RUN_IGNORED_TEST_TIMEOUT_SECS`-style callers elsewhere in the toolkit for
//! the same pattern: absence of a signal never blocks output, it just narrows
//! it).

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowConfig {
    /// Window length in hours, as reported by the user's own `/usage` view.
    pub hours: f64,
    /// RFC3339 timestamp of the last known window reset.
    pub last_reset: String,
}

fn file_path(dir: &Path) -> PathBuf {
    dir.join("window.json")
}

/// Load the registered window config from `dir` (normally `config::base_dir()`).
/// Returns `None` on any failure (missing file, bad JSON) — never errors, since
/// every caller treats "unset" as a normal, expected state.
pub fn load(dir: &Path) -> Option<WindowConfig> {
    let text = std::fs::read_to_string(file_path(dir)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Persist `cfg` under `dir`, creating the directory if needed.
pub fn save(dir: &Path, cfg: &WindowConfig) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    let text = serde_json::to_string_pretty(cfg)?;
    std::fs::write(file_path(dir), text)?;
    Ok(())
}

/// Approximate seconds remaining until the next window reset, given the
/// current time. `None` when `hours` is non-positive or `last_reset` doesn't
/// parse — callers fall back to showing uptime only.
pub fn approx_reset_in_secs(cfg: &WindowConfig, now: DateTime<Utc>) -> Option<i64> {
    if cfg.hours <= 0.0 {
        return None;
    }
    let reset = DateTime::parse_from_rfc3339(&cfg.last_reset)
        .ok()?
        .with_timezone(&Utc);
    let window_secs = (cfg.hours * 3600.0).round() as i64;
    if window_secs <= 0 {
        return None;
    }
    let elapsed = (now - reset).num_seconds().max(0);
    Some(window_secs - (elapsed % window_secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_missing_file_is_none() {
        let dir =
            std::env::temp_dir().join(format!("gauge-window-test-missing-{}", std::process::id()));
        assert!(load(&dir).is_none());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = std::env::temp_dir().join(format!(
            "gauge-window-test-roundtrip-{}-{}",
            std::process::id(),
            "a"
        ));
        let cfg = WindowConfig {
            hours: 5.0,
            last_reset: "2026-07-16T00:00:00+00:00".to_string(),
        };
        save(&dir, &cfg).expect("save should succeed");
        let loaded = load(&dir).expect("load should find the saved file");
        assert_eq!(loaded, cfg);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_unparseable_json_is_none() {
        let dir = std::env::temp_dir().join(format!(
            "gauge-window-test-bad-json-{}-{}",
            std::process::id(),
            "b"
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(file_path(&dir), "not json").unwrap();
        assert!(load(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn approx_reset_in_secs_computes_remaining_time_in_current_window() {
        let cfg = WindowConfig {
            hours: 5.0,
            last_reset: "2026-07-16T00:00:00+00:00".to_string(),
        };
        // 1 hour into a 5-hour window → 4 hours (14400s) remaining.
        let now = DateTime::parse_from_rfc3339("2026-07-16T01:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(approx_reset_in_secs(&cfg, now), Some(4 * 3600));
    }

    #[test]
    fn approx_reset_in_secs_wraps_across_multiple_elapsed_windows() {
        let cfg = WindowConfig {
            hours: 5.0,
            last_reset: "2026-07-16T00:00:00+00:00".to_string(),
        };
        // 11 hours elapsed = 2 full 5h windows + 1h into the third → 4h remaining.
        let now = DateTime::parse_from_rfc3339("2026-07-16T11:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(approx_reset_in_secs(&cfg, now), Some(4 * 3600));
    }

    #[test]
    fn approx_reset_in_secs_none_for_non_positive_hours() {
        let cfg = WindowConfig {
            hours: 0.0,
            last_reset: "2026-07-16T00:00:00+00:00".to_string(),
        };
        assert!(approx_reset_in_secs(&cfg, Utc::now()).is_none());
    }

    #[test]
    fn approx_reset_in_secs_none_for_unparseable_last_reset() {
        let cfg = WindowConfig {
            hours: 5.0,
            last_reset: "not-a-date".to_string(),
        };
        assert!(approx_reset_in_secs(&cfg, Utc::now()).is_none());
    }
}
