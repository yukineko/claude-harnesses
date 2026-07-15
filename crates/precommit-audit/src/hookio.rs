//! Hook plumbing: stdin protocol, state files, audit log. These conventions
//! follow Claude Code's hook contract but are inert when the tool is run as a
//! plain pre-commit-framework hook (no stdin JSON, no review artifact).

use std::path::{Path, PathBuf};

/// Parsed subset of the Claude Code Stop-hook stdin payload.
pub struct HookInput {
    pub stop_hook_active: bool,
    /// `hook_event_name` from the payload (e.g. "Stop", "SessionEnd"). Empty
    /// when absent (e.g. a plain pre-commit-framework invocation with no JSON).
    pub event: String,
}

/// Read and parse stdin JSON. Returns a default (non-recursive) input when
/// stdin is empty or unparseable, so non-Claude invocations Just Work.
pub fn read_stdin() -> HookInput {
    let raw = harness_core::hook::read_stdin();
    if raw.trim().is_empty() {
        return HookInput {
            stop_hook_active: false,
            event: String::new(),
        };
    }
    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(v) => HookInput {
            stop_hook_active: v
                .get("stop_hook_active")
                .and_then(|b| b.as_bool())
                .unwrap_or(false),
            event: v
                .get("hook_event_name")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string(),
        },
        Err(_) => HookInput {
            stop_hook_active: false,
            event: String::new(),
        },
    }
}

/// One-shot escape hatch: `<audit_dir>/.audit-skip`. If present, consume it
/// (delete), clear the block marker, and return the reason string.
pub fn consume_skip(root: &Path, audit_dir: &str) -> Option<String> {
    let skip = root.join(audit_dir).join(".audit-skip");
    if !skip.exists() {
        return None;
    }
    let reason = std::fs::read_to_string(&skip)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "(no reason given)".to_string());
    let _ = std::fs::remove_file(&skip);
    let _ = std::fs::remove_file(block_marker(root, audit_dir));
    Some(reason)
}

pub fn block_marker(root: &Path, audit_dir: &str) -> PathBuf {
    root.join(audit_dir).join(".audit-blocked")
}

/// Create the block marker (best effort; parent dir must already exist).
pub fn set_block_marker(root: &Path, audit_dir: &str) {
    let p = block_marker(root, audit_dir);
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&p, b"");
}

pub fn clear_block_marker(root: &Path, audit_dir: &str) {
    let _ = std::fs::remove_file(block_marker(root, audit_dir));
}

/// Append a JSONL entry to `<audit_dir>/audit-log.jsonl`. Best effort.
// The fields are an audit record's columns; bundling them into a struct would
// just move the same arity to the call site.
#[allow(clippy::too_many_arguments)]
pub fn write_audit_log(
    root: &Path,
    audit_dir: &str,
    mode: &str,
    verdict: &str,
    issue_count: usize,
    categories: &[String],
    warning_count: usize,
    changed_count: usize,
    timestamp: &str,
) {
    let entry = serde_json::json!({
        "ts": timestamp,
        "event": "pre-commit-audit",
        "mode": mode,
        "verdict": verdict,
        "issueCount": issue_count,
        "issueCategories": categories,
        "warningCount": warning_count,
        "changedCount": changed_count,
    });
    let path = root.join(audit_dir).join("audit-log.jsonl");
    if let Ok(line) = serde_json::to_string(&entry) {
        harness_core::append::append_line(&path, &line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_audit_log_appends_one_parseable_json_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_audit_log(
            dir.path(),
            ".precommit-audit",
            "check",
            "pass",
            0,
            &[],
            0,
            3,
            "2026-07-15T00:00:00Z",
        );
        let path = dir.path().join(".precommit-audit").join("audit-log.jsonl");
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1);
        let v: serde_json::Value = serde_json::from_str(lines[0]).expect("one JSON object");
        assert_eq!(v["verdict"], "pass");
        assert_eq!(v["changedCount"], 3);
    }

    /// Regression guard: `write_audit_log` used to append via
    /// `writeln!(f, "{line}")`, which splits the JSON body and the trailing
    /// `\n` into two separate `write()` syscalls. Under concurrent
    /// `O_APPEND` writers (e.g. parallel hook invocations racing on the same
    /// audit log) that split lets two writers' bodies land back-to-back
    /// before either newline arrives, concatenating two JSON objects onto
    /// one physical line and corrupting the JSONL log. Routing through
    /// `harness_core::append::append_line` (single `write_all` of body+`\n`)
    /// must keep every record on its own parseable line even under heavy
    /// concurrent writing.
    #[test]
    fn concurrent_write_audit_log_never_interleaves_records() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let audit_dir = ".precommit-audit";

        const THREADS: usize = 8;
        const PER_THREAD: usize = 300;

        std::thread::scope(|scope| {
            for t in 0..THREADS {
                let root = root.clone();
                scope.spawn(move || {
                    for i in 0..PER_THREAD {
                        // Vary the category list length so payload size
                        // varies too, exercising different write offsets.
                        let categories: Vec<String> =
                            (0..(i % 5)).map(|k| format!("cat-{t}-{k}")).collect();
                        write_audit_log(
                            &root,
                            audit_dir,
                            "check",
                            "pass",
                            i,
                            &categories,
                            0,
                            i,
                            "2026-07-15T00:00:00Z",
                        );
                    }
                });
            }
        });

        let path = root.join(audit_dir).join("audit-log.jsonl");
        let text = std::fs::read_to_string(&path).unwrap();
        let mut count = 0usize;
        for (n, l) in text.lines().enumerate() {
            if l.is_empty() {
                continue;
            }
            serde_json::from_str::<serde_json::Value>(l)
                .unwrap_or_else(|e| panic!("line {n} is not a single JSON object: {e} — {l:.160}"));
            count += 1;
        }
        assert_eq!(
            count,
            THREADS * PER_THREAD,
            "every write_audit_log call must survive as exactly one parseable line"
        );
    }
}
