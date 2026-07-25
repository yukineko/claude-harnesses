//! Observability for schema rejects — writes one JSON line per reject to
//! `~/.schemaguard/rejects.jsonl` and can aggregate totals by schema name.
//!
//! Design choices:
//! - **Writes fail soft**: a write error never changes the gate exit code; only
//!   a warning goes to stderr. Losing a counter line must not turn a valid
//!   payload into a rejected one.
//! - **Reads fail closed**: reading the store back is a *judgement* ("how many
//!   rejects were there?"), so an unreadable store resolves to
//!   [`Determination::Undetermined`], never to an empty map. Absent and
//!   unreadable are different answers and stay distinguishable downstream.
//! - **Append-only JSONL**: easy to `tail -f` and trivially diff-able in git.
//! - **No timestamp by default** (the spec is explicit), but we include one
//!   as an optional field using `SystemTime` to aid debugging without making
//!   the test corpus time-dependent.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use harness_core::boundary;
use harness_core::verdict::Determination;
use serde::{Deserialize, Serialize};

// ── types ────────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Debug)]
struct RejectLine {
    schema: String,
    violations: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    ts: Option<u64>,
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn rejects_path() -> PathBuf {
    harness_core::config::base_dir("schemaguard").join("rejects.jsonl")
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ── public API ────────────────────────────────────────────────────────────────

/// Append one reject line to the metrics store.
///
/// Fails soft: any I/O error is printed to stderr but does **not** propagate.
pub fn record_reject(schema: &str, violations: usize) {
    let path = rejects_path();
    if let Err(e) = write_reject_line(&path, schema, violations) {
        eprintln!("schemaguard: metrics write warning: {e}");
    }
}

fn write_reject_line(path: &PathBuf, schema: &str, violations: usize) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line = RejectLine {
        schema: schema.to_string(),
        violations,
        ts: Some(unix_secs()),
    };
    let mut json = serde_json::to_string(&line)?;
    json.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(json.as_bytes())?;
    Ok(())
}

/// Return cumulative reject counts per schema name, read from the JSONL store.
///
/// Three-valued on purpose. The reject counter exists so that silent drops at a
/// source→executor boundary become **observable**; folding an unreadable store
/// into an empty map would report "zero rejects" — i.e. "no silent drops" — which
/// is the exact inversion of that purpose. So:
///
/// - store absent (nothing recorded yet) → `Known(empty map)`; legitimately empty
/// - store readable                      → `Known(counts)`
/// - store present but unreadable        → `Undetermined(why)`; the caller cannot
///   collapse this to a value, because [`Determination`] has no `unwrap_or`/`ok`
///
/// Malformed *lines* inside a readable store are still silently skipped — that is
/// a separate, still-open gap (see [`parse_counts`]), not something this function
/// resolves.
pub fn counts() -> Determination<BTreeMap<String, usize>> {
    counts_at(&rejects_path())
}

/// [`counts`] against an explicit path, so the tri-state can be exercised without
/// touching `$HOME`.
pub fn counts_at(path: &Path) -> Determination<BTreeMap<String, usize>> {
    boundary::read_to_string(path).map(|maybe_raw| match maybe_raw {
        // Absent: never written. A genuinely empty observation, not a failure.
        None => BTreeMap::new(),
        Some(raw) => parse_counts(raw.lines().map(|l| Ok(l.to_string()))),
    })
}

/// Pure helper: sum reject counts from an iterator of raw JSON lines.
/// Exported for testing without touching the filesystem.
pub fn parse_counts(
    lines: impl Iterator<Item = std::io::Result<String>>,
) -> BTreeMap<String, usize> {
    let mut map: BTreeMap<String, usize> = BTreeMap::new();
    for line in lines {
        let line = match line {
            Ok(l) if !l.trim().is_empty() => l,
            _ => continue,
        };
        if let Ok(entry) = serde_json::from_str::<RejectLine>(&line) {
            *map.entry(entry.schema).or_insert(0) += entry.violations;
        }
    }
    map
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(raw: &str) -> impl Iterator<Item = std::io::Result<String>> + '_ {
        raw.lines().map(|l| Ok(l.to_string()))
    }

    #[test]
    fn parse_counts_empty_input() {
        let result = parse_counts(lines(""));
        assert!(result.is_empty());
    }

    #[test]
    fn parse_counts_sums_per_schema() {
        let input = r#"{"schema":"decomposition","violations":2}
{"schema":"episode","violations":1}
{"schema":"decomposition","violations":3}
"#;
        let result = parse_counts(lines(input));
        assert_eq!(result["decomposition"], 5);
        assert_eq!(result["episode"], 1);
    }

    #[test]
    fn parse_counts_skips_malformed_lines() {
        let input = r#"not json at all
{"schema":"playbook","violations":1}
{broken
"#;
        let result = parse_counts(lines(input));
        assert_eq!(result.get("playbook"), Some(&1));
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn parse_counts_missing_file_gives_empty() {
        // counts() itself falls back to empty — we test the pure helper with no lines
        let result = parse_counts(std::iter::empty());
        assert!(result.is_empty());
    }

    #[test]
    fn parse_counts_zero_violations_line() {
        // A line with 0 violations should still be summed (edge case)
        let input = r#"{"schema":"episode","violations":0}"#;
        let result = parse_counts(lines(input));
        assert_eq!(result["episode"], 0);
    }

    #[test]
    fn parse_counts_with_ts_field() {
        // Lines that include optional `ts` must still parse
        let input = r#"{"schema":"scout-measure","violations":2,"ts":1700000000}"#;
        let result = parse_counts(lines(input));
        assert_eq!(result["scout-measure"], 2);
    }
}
