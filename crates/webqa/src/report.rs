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
    /// Per reported file: how many of its tests actually ran (status other
    /// than `skipped`). Collected from every suite level.
    pub file_tests: Vec<(String, u64)>,
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
    let mut file_tests: Vec<(String, u64)> = Vec::new();
    if let Some(suites) = v.get("suites").and_then(Value::as_array) {
        for s in suites {
            collect_failed(s, &mut failed_titles);
            collect_files(s, None, &mut file_tests);
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
        file_tests,
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

fn add_file(out: &mut Vec<(String, u64)>, file: &str, n: u64) {
    match out.iter_mut().find(|(f, _)| f == file) {
        Some((_, c)) => *c += n,
        None => out.push((file.to_string(), n)),
    }
}

fn collect_files(suite: &Value, parent: Option<&str>, out: &mut Vec<(String, u64)>) {
    let file = suite.get("file").and_then(Value::as_str).or(parent);
    if let Some(f) = file {
        add_file(out, f, 0);
    }
    if let Some(specs) = suite.get("specs").and_then(Value::as_array) {
        for sp in specs {
            let Some(f) = sp.get("file").and_then(Value::as_str).or(file) else {
                continue;
            };
            let ran = sp.get("tests").and_then(Value::as_array).map_or(0, |ts| {
                ts.iter()
                    .filter(|t| {
                        t.get("status")
                            .and_then(Value::as_str)
                            .is_some_and(|s| s != "skipped")
                    })
                    .count()
            });
            add_file(out, f, ran as u64);
        }
    }
    if let Some(children) = suite.get("suites").and_then(Value::as_array) {
        for c in children {
            collect_files(c, file, out);
        }
    }
}

/// Last `n` lines of `s`.
pub fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}
