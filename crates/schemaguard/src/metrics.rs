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
/// - store readable but with any unreadable/malformed line → `Undetermined(why)`;
///   the sum over the remaining lines is an undercount, and presenting it as a
///   count is the silent drop this counter exists to expose (backlog 27926f7e).
pub fn counts() -> Determination<BTreeMap<String, usize>> {
    counts_at(&rejects_path())
}

/// [`counts`] against an explicit path, so the tri-state can be exercised without
/// touching `$HOME`.
pub fn counts_at(path: &Path) -> Determination<BTreeMap<String, usize>> {
    match boundary::read_to_string(path) {
        // Absent: never written. A genuinely empty observation, not a failure.
        Determination::Known(None) => Determination::known(BTreeMap::new()),
        Determination::Known(Some(raw)) => {
            let parsed = parse_counts(raw.lines().map(|l| Ok(l.to_string())));
            if parsed.skipped == 0 {
                Determination::known(parsed.counts)
            } else {
                Determination::undetermined(format!(
                    "{} line(s) of {} could not be read or parsed; the remaining sum is an undercount",
                    parsed.skipped,
                    path.display()
                ))
            }
        }
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

/// Result of [`parse_counts`]: the sum over the lines that parsed, plus how many
/// non-blank lines were dropped (IO error or malformed JSON). A non-zero
/// `skipped` means `counts` is an undercount and must not be presented as a fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCounts {
    pub counts: BTreeMap<String, usize>,
    pub skipped: usize,
}

/// Pure helper: sum reject counts from an iterator of raw JSON lines.
/// Exported for testing without touching the filesystem.
///
/// Blank lines are not data and are ignored. An `Err` line or a non-blank line
/// that does not parse is COUNTED in `skipped`, never silently dropped.
pub fn parse_counts(lines: impl Iterator<Item = std::io::Result<String>>) -> ParsedCounts {
    let mut map: BTreeMap<String, usize> = BTreeMap::new();
    let mut skipped = 0usize;
    for line in lines {
        let line = match line {
            Ok(l) if l.trim().is_empty() => continue,
            Ok(l) => l,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        match serde_json::from_str::<RejectLine>(&line) {
            Ok(entry) => *map.entry(entry.schema).or_insert(0) += entry.violations,
            Err(_) => skipped += 1,
        }
    }
    ParsedCounts {
        counts: map,
        skipped,
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(raw: &str) -> impl Iterator<Item = std::io::Result<String>> + '_ {
        raw.lines().map(|l| Ok(l.to_string()))
    }

    /// backlog 27926f7e: a readable store that contains a malformed line is an
    /// UNDERCOUNT, not a count. Presenting the partial sum as `Known` is the
    /// silent-drop this counter exists to expose.
    #[test]
    fn counts_at_with_malformed_line_is_undetermined() {
        let dir = std::env::temp_dir().join(format!(
            "schemaguard-metrics-malformed-{}-{}",
            std::process::id(),
            unix_secs()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rejects.jsonl");
        std::fs::write(
            &path,
            "{\"schema\":\"playbook\",\"violations\":1}\n{broken\n",
        )
        .unwrap();
        let got = counts_at(&path);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
        assert!(
            matches!(got, Determination::Undetermined(_)),
            "a store with a malformed line must be Undetermined, got {got:?}"
        );
    }

    /// Anti-vacuity control: a fully well-formed store is still `Known`.
    #[test]
    fn counts_at_well_formed_store_is_known() {
        let dir = std::env::temp_dir().join(format!(
            "schemaguard-metrics-wellformed-{}-{}",
            std::process::id(),
            unix_secs()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rejects.jsonl");
        std::fs::write(&path, "{\"schema\":\"playbook\",\"violations\":1}\n\n").unwrap();
        let got = counts_at(&path);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
        let expected: BTreeMap<String, usize> = [("playbook".to_string(), 1)].into();
        assert_eq!(
            got,
            Determination::known(expected),
            "a well-formed store must be Known with its exact counts"
        );
    }

    #[test]
    fn parse_counts_empty_input() {
        let result = parse_counts(lines(""));
        assert!(result.counts.is_empty());
        assert_eq!(result.skipped, 0);
    }

    #[test]
    fn parse_counts_io_error_line_is_skipped() {
        let input = vec![
            Ok(r#"{"schema":"episode","violations":1}"#.to_string()),
            Err(std::io::Error::other("read failed")),
        ];
        let result = parse_counts(input.into_iter());
        assert_eq!(result.counts["episode"], 1);
        assert_eq!(result.skipped, 1, "an unreadable line must be counted");
    }

    #[test]
    fn parse_counts_sums_per_schema() {
        let input = r#"{"schema":"decomposition","violations":2}
{"schema":"episode","violations":1}
{"schema":"decomposition","violations":3}
"#;
        let result = parse_counts(lines(input));
        assert_eq!(result.counts["decomposition"], 5);
        assert_eq!(result.counts["episode"], 1);
    }

    #[test]
    fn parse_counts_counts_malformed_lines_as_skipped() {
        let input = r#"not json at all
{"schema":"playbook","violations":1}
{broken
"#;
        let result = parse_counts(lines(input));
        assert_eq!(result.counts.get("playbook"), Some(&1));
        assert_eq!(result.counts.len(), 1);
        assert_eq!(
            result.skipped, 2,
            "both malformed lines must be counted as skipped, not silently dropped"
        );
    }

    #[test]
    fn parse_counts_missing_file_gives_empty() {
        // an absent store is Known(empty) in counts_at; here: the pure helper with no lines
        let result = parse_counts(std::iter::empty());
        assert!(result.counts.is_empty());
        assert_eq!(result.skipped, 0);
    }

    #[test]
    fn parse_counts_zero_violations_line() {
        // A line with 0 violations should still be summed (edge case)
        let input = r#"{"schema":"episode","violations":0}"#;
        let result = parse_counts(lines(input));
        assert_eq!(result.counts["episode"], 0);
    }

    #[test]
    fn parse_counts_with_ts_field() {
        // Lines that include optional `ts` must still parse
        let input = r#"{"schema":"scout-measure","violations":2,"ts":1700000000}"#;
        let result = parse_counts(lines(input));
        assert_eq!(result.counts["scout-measure"], 2);
    }
}
