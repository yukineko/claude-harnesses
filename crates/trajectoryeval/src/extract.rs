//! Ordered tool-call extractor from a Claude Code transcript.
//!
//! A transcript is JSONL: one JSON object per line, each a transcript event. We
//! STREAM it line-by-line (the harness has a hard rule against loading a whole
//! transcript into memory) and collect, in order, the `name` of every
//! `type == "tool_use"` content block. A missing field is tolerated (that event
//! contributes nothing), but a line that doesn't parse as JSON, or a transcript
//! with no events at all, is an `InvalidData` error — never a silent skip and
//! never a panic.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde_json::Value;

/// Stream `path` and return the ordered list of `tool_use` names.
pub fn extract_tools(path: &Path) -> std::io::Result<Vec<String>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);

    let mut tools = Vec::new();
    let mut events = 0usize;
    for (idx, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        // A line that isn't valid JSON is an unparseable transcript, not an
        // event to skip: skipping could drop a tool_use and make the extracted
        // trajectory look cleaner than the session was (CLAUDE.md §3).
        let value: Value = serde_json::from_str(&line).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("line {} is not valid JSON: {}", idx + 1, e),
            )
        })?;
        events += 1;
        collect_from_event(&value, &mut tools);
    }
    // No event at all is not "a session with no tool calls" — it is no
    // transcript. Report it rather than emitting an empty trajectory.
    if events == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "transcript contains no JSON events",
        ));
    }
    Ok(tools)
}

/// Pull tool_use names out of one event, tolerating shape variation.
fn collect_from_event(value: &Value, tools: &mut Vec<String>) {
    // Assistant messages keep their blocks under ["message"]["content"]; some
    // shapes put a content array at the top level. Tolerate both.
    if let Some(content) = value.get("message").and_then(|m| m.get("content")) {
        collect_from_content(content, tools);
    }
    if let Some(content) = value.get("content") {
        collect_from_content(content, tools);
    }
}

/// For each content block that is an object with `"type":"tool_use"`, push its name.
fn collect_from_content(content: &Value, tools: &mut Vec<String>) {
    let Some(arr) = content.as_array() else {
        return;
    };
    for block in arr {
        let is_tool_use = block.get("type").and_then(Value::as_str) == Some("tool_use");
        if is_tool_use {
            if let Some(name) = block.get("name").and_then(Value::as_str) {
                tools.push(name.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn extracts_tool_names_in_order() {
        // Temp fixture keyed by pid so parallel test runs don't collide.
        let mut path = std::env::temp_dir();
        path.push(format!(
            "trajectoryeval-fixture-{}.jsonl",
            std::process::id()
        ));

        let mut f = File::create(&path).unwrap();
        // line 1: assistant message with two tool_use blocks (message.content shape)
        writeln!(
            f,
            r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"hi"}},{{"type":"tool_use","name":"Read","input":{{}}}},{{"type":"tool_use","name":"Edit","input":{{}}}}]}}}}"#
        )
        .unwrap();
        // line 2: a non-tool event (user message) — contributes nothing
        writeln!(
            f,
            r#"{{"type":"user","message":{{"content":"just text"}}}}"#
        )
        .unwrap();
        // line 3: top-level content shape with one tool_use
        writeln!(f, r#"{{"content":[{{"type":"tool_use","name":"Bash"}}]}}"#).unwrap();
        f.flush().unwrap();
        drop(f);

        let tools = extract_tools(&path).unwrap();
        assert_eq!(
            tools,
            vec!["Read".to_string(), "Edit".to_string(), "Bash".to_string()]
        );

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn unparseable_line_is_invalid_data_not_skipped() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "trajectoryeval-garbage-line-{}.jsonl",
            std::process::id()
        ));
        let mut f = File::create(&path).unwrap();
        writeln!(f, r#"{{"content":[{{"type":"tool_use","name":"Bash"}}]}}"#).unwrap();
        writeln!(f, "not json at all").unwrap();
        drop(f);
        let err = extract_tools(&path).unwrap_err();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn missing_file_is_io_error() {
        let mut path = std::env::temp_dir();
        path.push(format!("trajectoryeval-nope-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert!(extract_tools(&path).is_err());
    }
}
