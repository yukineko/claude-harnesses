//! The metrics SINK: an append-only JSONL writer shared by every plugin. Each
//! plugin keeps its own rollup/summary types (which fields it aggregates), but
//! the on-disk line schema and the parallel-safe append are defined once here.
//!
//! Each line is `{ts, session, event, ...extra}`, appended via
//! [`crate::append::append_line`] as a single atomic `O_APPEND` write well under
//! PIPE_BUF (4096B), so parallel sessions appending to one file don't interleave
//! (issue #15). Writes are best-effort and never break a hook: all errors are
//! swallowed.
//!
//! NOTE: a cross-plugin `plugin` field + a shared aggregator are Phase D work;
//! this keeps the existing per-plugin row schema unchanged.

use std::path::Path;

use serde_json::{json, Value};

/// ISO-8601 local timestamp with offset, e.g. `2026-06-22T13:24:05+09:00`.
pub fn now_iso() -> String {
    chrono::Local::now()
        .format("%Y-%m-%dT%H:%M:%S%:z")
        .to_string()
}

/// Append one event line `{ts, session, event, ...extra}` to `sink`. `extra` must
/// be a JSON object; non-object values contribute nothing. The parent dir is
/// created on demand. All errors are swallowed.
pub fn emit(sink: &Path, session: &str, event: &str, extra: Value) {
    let mut obj = serde_json::Map::new();
    obj.insert("ts".into(), json!(now_iso()));
    obj.insert("session".into(), json!(session));
    obj.insert("event".into(), json!(event));
    if let Value::Object(m) = extra {
        for (k, v) in m {
            obj.insert(k, v);
        }
    }
    let line = Value::Object(obj).to_string();

    // One record = one atomic write (body + '\n' in a single buffer). A prior
    // `writeln!` split the newline into a second syscall, letting concurrent
    // `O_APPEND` writers concatenate two JSON objects onto one line (issue #15).
    crate::append::append_line(sink, &line);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_writes_one_line_with_core_fields() {
        let sink = std::env::temp_dir()
            .join(format!("harness-metrics-{}", std::process::id()))
            .join("metrics.jsonl");
        let _ = std::fs::remove_file(&sink);

        emit(
            &sink,
            "S1",
            "budget",
            json!({"est_tokens": 100_000, "band": 1}),
        );
        emit(&sink, "S1", "rescue", json!({"note_bytes": 2048}));

        let text = std::fs::read_to_string(&sink).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let first: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["session"], "S1");
        assert_eq!(first["event"], "budget");
        assert_eq!(first["est_tokens"], 100_000);
        assert!(first["ts"].as_str().is_some());

        let _ = std::fs::remove_dir_all(sink.parent().unwrap());
    }
}
