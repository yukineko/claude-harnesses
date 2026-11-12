//! Extract the text of the final main-session assistant answer from a Claude
//! Code transcript (JSONL).

use std::io::{BufRead, BufReader};

use serde_json::Value;

/// The text blocks of one entry, joined by `\n`, when it is a non-sidechain
/// assistant entry with at least one non-empty text block.
fn assistant_text(v: &Value) -> Option<String> {
    if v.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let content = v.get("message")?.get("content")?;
    if let Some(s) = content.as_str() {
        return (!s.trim().is_empty()).then(|| s.to_string());
    }
    let texts: Vec<&str> = content
        .as_array()?
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .filter(|t| !t.trim().is_empty())
        .collect();
    (!texts.is_empty()).then(|| texts.join("\n"))
}

/// `Err` (always naming the transcript) when the path is empty, the file
/// cannot be read, no assistant text exists, or a line AFTER the last
/// assistant text cannot be parsed (it might be a later answer).
pub fn last_assistant_text(path: &str) -> Result<String, String> {
    if path.trim().is_empty() {
        return Err("the Stop payload has no transcript_path".to_string());
    }
    let f = std::fs::File::open(path)
        .map_err(|e| format!("the transcript {path} could not be opened: {e}"))?;
    let mut found: Option<(usize, String)> = None;
    let mut last_bad: Option<usize> = None;
    for (n, line) in BufReader::new(f).lines().enumerate() {
        let line = line.map_err(|e| {
            format!(
                "the transcript {path} could not be read at line {}: {e}",
                n + 1
            )
        })?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(&line) {
            Ok(v) => {
                if let Some(t) = assistant_text(&v) {
                    found = Some((n, t));
                }
            }
            Err(_) => last_bad = Some(n),
        }
    }
    match (found, last_bad) {
        (Some((f, _)), Some(b)) if b > f => Err(format!(
            "the transcript {path} line {} could not be parsed and comes after the last assistant text",
            b + 1
        )),
        (Some((_, t)), _) => Ok(t),
        (None, Some(b)) => Err(format!(
            "no assistant text found in the transcript {path} (line {} could not be parsed)",
            b + 1
        )),
        (None, None) => Err(format!("no assistant text found in the transcript {path}")),
    }
}
