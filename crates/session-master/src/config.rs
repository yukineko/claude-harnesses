//! Environment-derived configuration (SPEC §1).
//!
//! Every value that the role decision depends on is resolved here. An invalid
//! `SESSION_MASTER_NOW` is a cannot-determine (`Err`), never "use the wall
//! clock instead": a test or operator that injected a clock and got it wrong
//! must not silently be judged against a different time.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub const TTL_DEFAULT: i64 = 900;
pub const TTL_MIN: i64 = 30;
pub const TTL_MAX: i64 = 86_400;

/// State root: `SESSION_MASTER_HOME`, else `~/.session-master`.
pub fn state_root() -> Result<PathBuf, String> {
    if let Some(v) = std::env::var_os("SESSION_MASTER_HOME") {
        if !v.is_empty() {
            return Ok(PathBuf::from(v));
        }
    }
    match std::env::var_os("HOME") {
        Some(h) if !h.is_empty() => Ok(PathBuf::from(h).join(".session-master")),
        _ => Err("HOME も SESSION_MASTER_HOME も無いので状態ルートを決められない".to_string()),
    }
}

/// Current unix time (seconds), honouring `SESSION_MASTER_NOW`.
pub fn now() -> Result<i64, String> {
    match std::env::var("SESSION_MASTER_NOW") {
        Ok(v) => v
            .trim()
            .parse::<i64>()
            .map_err(|e| format!("SESSION_MASTER_NOW={v:?} が整数でない ({e})")),
        Err(std::env::VarError::NotPresent) => match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_secs()).map_err(|e| format!("時刻が範囲外 ({e})")),
            Err(e) => Err(format!("システム時刻が epoch より前 ({e})")),
        },
        Err(e) => Err(format!("SESSION_MASTER_NOW を読めない ({e})")),
    }
}

/// Heartbeat TTL, clamped to `[TTL_MIN, TTL_MAX]` (a floor, so a tiny value
/// cannot make every master look dead). Invalid → default.
pub fn ttl_secs() -> i64 {
    match std::env::var("SESSION_MASTER_TTL_SECS") {
        Ok(v) => match v.trim().parse::<i64>() {
            Ok(n) => n.clamp(TTL_MIN, TTL_MAX),
            Err(_) => TTL_DEFAULT,
        },
        Err(_) => TTL_DEFAULT,
    }
}

/// Branch names treated as "main" (comma separated). Empty / unset → `main`.
pub fn main_branches() -> Vec<String> {
    // Unset (or non-UTF-8) falls back to the documented default `main`.
    let mut v: Vec<String> = Vec::new();
    if let Ok(raw) = std::env::var("SESSION_MASTER_MAIN_BRANCHES") {
        v = raw
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    if v.is_empty() {
        vec!["main".to_string()]
    } else {
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttl_clamp_has_floor() {
        assert_eq!(5i64.clamp(TTL_MIN, TTL_MAX), TTL_MIN);
        assert_eq!(1_000_000i64.clamp(TTL_MIN, TTL_MAX), TTL_MAX);
    }
}
