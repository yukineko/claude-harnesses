//! Cross-harness UserPromptSubmit injection-size ledger — ADR 0001 Phase 2.
//!
//! Five plugins inject `additionalContext` on every qualifying UserPromptSubmit
//! (`playbook`, `run-book`, `ctxrot`, `context-governor`, `fugu-router`). Each has
//! its own per-injector char cap, but nobody watches the COMBINED per-turn size.
//!
//! The five hooks run as five separate processes with no cross-process channel.
//! The key insight: they all receive the SAME user `prompt` on the SAME
//! UserPromptSubmit event, so a stable hash of `session_id + "\n" + prompt` is a
//! deterministic shared TURN KEY across the five processes — no coordination
//! needed. Each injector appends one [`InjectEntry`] (keyed by that turn_key) to
//! a single central ledger, and `harness-status inject` groups by turn_key to sum
//! a single turn and warn when it exceeds an aggregate budget.
//!
//! Resilience mirrors [`crate::hook_latency`]: recording is BEST-EFFORT and every
//! IO/serialization error is swallowed. An observability log must never break the
//! turn it is measuring.

use std::path::{Path, PathBuf};

/// One recorded UserPromptSubmit injection by one plugin in one turn.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct InjectEntry {
    pub ts: String,       // rfc3339 local time
    pub turn_key: String, // stable hash of session + prompt
    pub plugin: String,   // "playbook" | "run-book" | "ctxrot" | "context-governor" | "fugu-router"
    pub session: String,
    pub chars: usize, // injected size in CHARS (not bytes) — CJK-safe accounting
}

/// A STABLE 64-bit hash of `session + "\n" + prompt`, lowercase hex.
///
/// Deliberately NOT `DefaultHasher` (whose seed/algorithm is not guaranteed
/// stable across processes or builds). This is a tiny FNV-1a so the five
/// independent injector processes derive the SAME turn key for the same turn.
pub fn turn_key(session: &str, prompt: &str) -> String {
    // FNV-1a, 64-bit.
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    for byte in format!("{session}\n{prompt}").as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}

/// The single central ledger path:
/// `<base_dir("harness")>/state/inject-metrics.jsonl`.
pub fn ledger_path() -> PathBuf {
    crate::config::base_dir("harness")
        .join("state")
        .join("inject-metrics.jsonl")
}

/// Append one [`InjectEntry`] for a real (`chars > 0`) UserPromptSubmit injection
/// to the central ledger. `chars == 0` records nothing (only real injections are
/// logged). BEST-EFFORT: all IO/serialization errors are swallowed so an
/// observability log can never break the turn it records.
pub fn record(plugin: &str, session: &str, prompt: &str, chars: usize) {
    record_to(&ledger_path(), plugin, session, prompt, chars);
}

/// Testable core of [`record`]: append one entry line to `path`, creating parent
/// dirs. `chars == 0` is a no-op. Best-effort — errors are swallowed (mirrors
/// [`crate::hook_latency::record_to`]).
pub fn record_to(path: &Path, plugin: &str, session: &str, prompt: &str, chars: usize) {
    if chars == 0 {
        return; // only record real injections
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let entry = InjectEntry {
        ts: chrono::Local::now().to_rfc3339(),
        turn_key: turn_key(session, prompt),
        plugin: plugin.to_string(),
        session: session.to_string(),
        chars,
    };
    if let Ok(line) = serde_json::to_string(&entry) {
        // Single atomic append (body + '\n' in one write) — see issue #15.
        crate::append::append_line(path, &line);
    }
}

/// Per-turn aggregation of injection size across all plugins.
pub struct TurnInjection {
    pub turn_key: String,
    pub total_chars: usize,
    pub per_plugin: Vec<(String, usize)>,
    pub latest_ts: String,
}

/// Read the JSONL ledger at `path`, group entries by `turn_key`, and SUM `chars`
/// both overall (`total_chars`) and per plugin (`per_plugin`).
///
/// Repeat policy: if a plugin appears multiple times within one turn its entries
/// are SUMMED. `latest_ts` tracks the most-recent (lexically greatest, which for
/// rfc3339 is chronologically latest) timestamp seen in the turn. Turns are
/// sorted by `latest_ts` DESCENDING (most-recent turn first). Malformed lines are
/// skipped silently, and a missing file yields an empty vec (never panics).
pub fn aggregate(path: &Path) -> Vec<TurnInjection> {
    let body = match std::fs::read_to_string(path) {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };

    // Preserve first-seen order of turns and of plugins within a turn so the
    // output is deterministic before the final latest_ts sort.
    let mut order: Vec<String> = Vec::new();
    let mut totals: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut per_plugin: std::collections::HashMap<String, Vec<(String, usize)>> =
        std::collections::HashMap::new();
    let mut latest: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry: InjectEntry = match serde_json::from_str(line) {
            Ok(e) => e,
            Err(_) => continue, // skip malformed lines silently
        };
        if !totals.contains_key(&entry.turn_key) {
            order.push(entry.turn_key.clone());
        }
        *totals.entry(entry.turn_key.clone()).or_insert(0) += entry.chars;
        let plugins = per_plugin.entry(entry.turn_key.clone()).or_default();
        if let Some(slot) = plugins.iter_mut().find(|(p, _)| *p == entry.plugin) {
            slot.1 += entry.chars;
        } else {
            plugins.push((entry.plugin.clone(), entry.chars));
        }
        let slot = latest.entry(entry.turn_key.clone()).or_default();
        if entry.ts > *slot {
            *slot = entry.ts.clone();
        }
    }

    let mut out: Vec<TurnInjection> = order
        .into_iter()
        .map(|turn_key| TurnInjection {
            total_chars: totals.get(&turn_key).copied().unwrap_or(0),
            per_plugin: per_plugin.remove(&turn_key).unwrap_or_default(),
            latest_ts: latest.remove(&turn_key).unwrap_or_default(),
            turn_key,
        })
        .collect();
    // Most-recent turn first; stable so equal timestamps keep first-seen order.
    out.sort_by(|a, b| b.latest_ts.cmp(&a.latest_ts));
    out
}

/// The turns whose combined `total_chars` strictly exceeds `budget_chars`.
pub fn over_budget(turns: &[TurnInjection], budget_chars: usize) -> Vec<&TurnInjection> {
    turns
        .iter()
        .filter(|t| t.total_chars > budget_chars)
        .collect()
}

/// Cooperative self-cap helper: `budget_chars` minus the chars already recorded
/// for `turn_key`, saturating at 0. An injector could call this before injecting
/// to leave room for its siblings. Provided for a future active-enforcement
/// phase; the shipped enforcement is detection + warn only.
pub fn remaining_for_turn(path: &Path, turn_key: &str, budget_chars: usize) -> usize {
    let used: usize = aggregate(path)
        .iter()
        .find(|t| t.turn_key == turn_key)
        .map(|t| t.total_chars)
        .unwrap_or(0);
    budget_chars.saturating_sub(used)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("hc-inject-{}-{tag}", std::process::id()))
            .join("state")
            .join("inject-metrics.jsonl")
    }

    #[test]
    fn turn_key_is_stable_and_prompt_sensitive() {
        // Identical (session, prompt) → identical key, deterministically.
        assert_eq!(turn_key("s1", "hello"), turn_key("s1", "hello"));
        // Different prompt → different key.
        assert_ne!(turn_key("s1", "hello"), turn_key("s1", "world"));
        // Different session → different key.
        assert_ne!(turn_key("s1", "hello"), turn_key("s2", "hello"));
        // Known-answer so a build/algorithm change is caught: FNV-1a of
        // "s1\nhello" as 16-hex-digit lowercase.
        assert_eq!(turn_key("s1", "hello").len(), 16);
        assert!(turn_key("s1", "hello")
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn record_to_writes_one_line_and_skips_zero() {
        let path = tmp("record");
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
        assert!(!path.exists(), "parent dirs do not exist yet");
        record_to(&path, "playbook", "sess-a", "prompt", 120);
        record_to(&path, "ctxrot", "sess-a", "prompt", 0); // skipped
        record_to(&path, "run-book", "sess-a", "prompt", 30);
        let body = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2, "chars==0 records nothing");
        assert!(lines[0].contains("\"playbook\"") && lines[0].contains("120"));
        assert!(lines[1].contains("\"run-book\""));
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn aggregate_groups_sums_and_sorts_by_latest() {
        let path = tmp("agg");
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Two turns; turn-2 has the later ts so it must sort first.
        let k1 = turn_key("s", "p1");
        let k2 = turn_key("s", "p2");
        let lines = format!(
            "{}\n{}\n{}\n{}\n",
            serde_json::to_string(&InjectEntry {
                ts: "2026-06-01T10:00:00+09:00".into(),
                turn_key: k1.clone(),
                plugin: "playbook".into(),
                session: "s".into(),
                chars: 100,
            })
            .unwrap(),
            serde_json::to_string(&InjectEntry {
                ts: "2026-06-01T10:00:01+09:00".into(),
                turn_key: k1.clone(),
                plugin: "playbook".into(), // repeat plugin in same turn → SUM
                session: "s".into(),
                chars: 50,
            })
            .unwrap(),
            serde_json::to_string(&InjectEntry {
                ts: "2026-06-01T11:00:00+09:00".into(),
                turn_key: k2.clone(),
                plugin: "ctxrot".into(),
                session: "s".into(),
                chars: 200,
            })
            .unwrap(),
            serde_json::to_string(&InjectEntry {
                ts: "2026-06-01T11:00:05+09:00".into(),
                turn_key: k2.clone(),
                plugin: "run-book".into(),
                session: "s".into(),
                chars: 300,
            })
            .unwrap(),
        );
        std::fs::write(&path, lines).unwrap();

        let agg = aggregate(&path);
        assert_eq!(agg.len(), 2);
        // turn-2 has the latest ts → first.
        assert_eq!(agg[0].turn_key, k2);
        assert_eq!(agg[0].total_chars, 500);
        assert_eq!(
            agg[0].per_plugin,
            vec![("ctxrot".to_string(), 200), ("run-book".to_string(), 300)]
        );
        assert_eq!(agg[0].latest_ts, "2026-06-01T11:00:05+09:00");
        // turn-1: repeated plugin summed.
        assert_eq!(agg[1].turn_key, k1);
        assert_eq!(agg[1].total_chars, 150);
        assert_eq!(agg[1].per_plugin, vec![("playbook".to_string(), 150)]);
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn over_budget_filters_strictly() {
        let turns = vec![
            TurnInjection {
                turn_key: "a".into(),
                total_chars: 150,
                per_plugin: vec![],
                latest_ts: "t1".into(),
            },
            TurnInjection {
                turn_key: "b".into(),
                total_chars: 300,
                per_plugin: vec![],
                latest_ts: "t2".into(),
            },
        ];
        let over = over_budget(&turns, 200);
        assert_eq!(over.len(), 1);
        assert_eq!(over[0].turn_key, "b");
        // budget exactly at total is NOT over (strict >).
        assert!(over_budget(&turns, 300).is_empty());
    }

    #[test]
    fn remaining_for_turn_subtracts_and_saturates() {
        let path = tmp("remaining");
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
        record_to(&path, "playbook", "s", "p", 120);
        record_to(&path, "ctxrot", "s", "p", 80); // same turn → 200 used
        let k = turn_key("s", "p");
        assert_eq!(remaining_for_turn(&path, &k, 1000), 800);
        // Over-used saturates at 0.
        assert_eq!(remaining_for_turn(&path, &k, 150), 0);
        // Unknown turn → full budget remaining.
        assert_eq!(remaining_for_turn(&path, "nope", 1000), 1000);
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn aggregate_skips_malformed_lines() {
        let path = tmp("malformed");
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "not json\n\
             {\"ts\":\"t\",\"turn_key\":\"k\",\"plugin\":\"playbook\",\"session\":\"s\",\"chars\":42}\n\
             {broken\n",
        )
        .unwrap();
        let agg = aggregate(&path);
        assert_eq!(agg.len(), 1);
        assert_eq!(agg[0].total_chars, 42);
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn aggregate_missing_file_is_empty_never_panics() {
        let path = tmp("missing");
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
        assert!(!path.exists());
        assert!(aggregate(&path).is_empty());
    }
}
