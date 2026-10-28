//! Parsing of the Playwright JSON reporter output and the block text.

use serde_json::Value;

#[derive(Debug)]
pub struct Stats {
    pub expected: u64,
    pub unexpected: u64,
    pub flaky: u64,
    pub skipped: u64,
    pub errors: usize,
    pub failed_titles: Vec<String>,
}

/// Strict parse: stdout must be exactly one JSON document with numeric
/// `stats.{expected,unexpected,flaky,skipped}`.
pub fn parse(stdout: &str) -> Result<Stats, String> {
    let v: Value =
        serde_json::from_str(stdout.trim()).map_err(|e| format!("stdout is not JSON: {e}"))?;
    let stats = v.get("stats").ok_or("JSON has no `stats` object")?;
    let n = |k: &str| -> Result<u64, String> {
        stats
            .get(k)
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("`stats.{k}` missing or not a non-negative integer"))
    };
    let mut failed_titles = Vec::new();
    if let Some(suites) = v.get("suites").and_then(Value::as_array) {
        for s in suites {
            collect_failed(s, &mut failed_titles);
        }
    }
    Ok(Stats {
        expected: n("expected")?,
        unexpected: n("unexpected")?,
        flaky: n("flaky")?,
        skipped: n("skipped")?,
        errors: v
            .get("errors")
            .and_then(Value::as_array)
            .map_or(0, Vec::len),
        failed_titles,
    })
}

fn collect_failed(suite: &Value, out: &mut Vec<String>) {
    if let Some(specs) = suite.get("specs").and_then(Value::as_array) {
        for sp in specs {
            if sp.get("ok").and_then(Value::as_bool) == Some(false) {
                let file = sp.get("file").and_then(Value::as_str).unwrap_or("?");
                let title = sp.get("title").and_then(Value::as_str).unwrap_or("?");
                out.push(format!("{file}: {title}"));
            }
        }
    }
    if let Some(children) = suite.get("suites").and_then(Value::as_array) {
        for c in children {
            collect_failed(c, out);
        }
    }
}

/// Last `n` lines of `s`.
pub fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}
