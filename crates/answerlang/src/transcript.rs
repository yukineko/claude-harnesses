//! Extract the text of the final main-session assistant answer from a Claude
//! Code transcript (JSONL).

use std::io::{BufRead, BufReader};

use serde_json::Value;

/// The last main-session assistant text of a transcript.
pub struct LastText {
    pub text: String,
    /// `true` when a non-sidechain `user` entry carrying a `tool_result` block
    /// comes AFTER `text`: the turn continued past this text, so it is an
    /// interim text and the final answer is evidently not (yet) in the file.
    pub stale: bool,
}

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

/// Whether the entry is a non-sidechain `user` entry with at least one
/// `tool_result` content block.
fn is_tool_result(v: &Value) -> bool {
    if v.get("type").and_then(Value::as_str) != Some("user") {
        return false;
    }
    if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return false;
    }
    v.get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks
                .iter()
                .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
        })
}

/// `Err` (always naming the transcript) when the path is empty, the file
/// cannot be read, no assistant text exists, or a line AFTER the last
/// assistant text cannot be parsed (it might be a later answer). `Ok` carries
/// the last assistant text and whether a `tool_result` follows it
/// ([`LastText::stale`]); the caller must not judge a stale text as final.
pub fn last_assistant_text(path: &str) -> Result<LastText, String> {
    if path.trim().is_empty() {
        return Err("the Stop payload has no transcript_path".to_string());
    }
    let f = std::fs::File::open(path)
        .map_err(|e| format!("the transcript {path} could not be opened: {e}"))?;
    let mut found: Option<(usize, String)> = None;
    let mut last_bad: Option<usize> = None;
    let mut last_tool_result: Option<usize> = None;
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
                } else if is_tool_result(&v) {
                    last_tool_result = Some(n);
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
        (Some((f, text)), _) => Ok(LastText {
            text,
            stale: last_tool_result.is_some_and(|r| r > f),
        }),
        (None, Some(b)) => Err(format!(
            "no assistant text found in the transcript {path} (line {} could not be parsed)",
            b + 1
        )),
        (None, None) => Err(format!("no assistant text found in the transcript {path}")),
    }
}
