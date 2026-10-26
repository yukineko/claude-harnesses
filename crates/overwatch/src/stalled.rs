//! Stalled-subagent scanner for `overwatch status` (backlog 938e1004).
//!
//! A subagent transcript
//! `$HOME/.claude/projects/<slug>/<session>/subagents/agent-<id>.jsonl` is
//! listed as stalled when its LAST JSONL entry is an assistant `tool_use` (so no
//! `tool_result` followed it) AND its mtime is at least [`STALL_SECS`] old.
//! Long-running commands and permission waits count too: this is report-only,
//! nothing is killed or restarted. The signal is the transcript (mtime + last
//! entry), never an output-file size.
//!
//! Fail-closed (CLAUDE.md section 3): a transcript that cannot be read, or whose
//! last entry cannot be parsed, is reported as [`StallState::Unknown`] with the
//! reason; it is never omitted and never treated as clean. Only a genuinely
//! absent projects/subagents directory (NotFound) means "nothing to list".

use harness_core::verdict::Determination;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Minimum transcript age (seconds) for a pending tool_use to count as stalled.
pub const STALL_SECS: u64 = 5 * 60;

/// Outcome for one listed transcript.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StallState {
    /// Pending tool_use, idle for `idle_minutes`.
    Stalled { idle_minutes: u64, pending: String },
    /// Could not be determined; `reason` says why.
    Unknown { reason: String },
}

/// One listed subagent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StalledSubagent {
    pub agent_id: String,
    #[serde(flatten)]
    pub state: StallState,
}

/// Claude Code project slug: every non-ASCII-alphanumeric char becomes `-`.
pub fn slug(project: &Path) -> String {
    project
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Scan the subagent transcripts of `project` under `home`.
pub fn scan(home: &Path, project: &Path, now: SystemTime) -> Vec<StalledSubagent> {
    let proj_dir = home.join(".claude/projects").join(slug(project));
    let mut out = Vec::new();
    let sessions = match read_dir_opt(&proj_dir) {
        Ok(v) => v,
        Err(why) => {
            out.push(unknown("(scan)", why));
            return out;
        }
    };
    for sess in sessions {
        let subdir = sess.join("subagents");
        let files = match read_dir_opt(&subdir) {
            Ok(v) => v,
            Err(why) => {
                out.push(unknown("(scan)", why));
                continue;
            }
        };
        for f in files {
            let Some(name) = f.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(id) = name
                .strip_prefix("agent-")
                .and_then(|r| r.strip_suffix(".jsonl"))
            else {
                continue;
            };
            if let Some(entry) = classify(&f, id, now) {
                out.push(entry);
            }
        }
    }
    out.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
    out
}

fn unknown(id: &str, reason: String) -> StalledSubagent {
    StalledSubagent {
        agent_id: id.to_string(),
        state: StallState::Unknown { reason },
    }
}

/// Directory listing where NotFound is a legitimate "nothing here" (empty);
/// every other failure is reported (never treated as empty).
fn read_dir_opt(dir: &Path) -> Result<Vec<PathBuf>, String> {
    match harness_core::boundary::read_dir_entries_checked(dir) {
        Determination::Known(v) => Ok(v.unwrap_or_default()),
        Determination::Undetermined(why) => Err(why.to_string()),
    }
}

/// `None` = observed and not stalled; `Some` = stalled or unknown.
fn classify(path: &Path, id: &str, now: SystemTime) -> Option<StalledSubagent> {
    let mtime = match std::fs::metadata(path).and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(e) => return Some(unknown(id, format!("cannot stat transcript: {e}"))),
    };
    let text = match harness_core::boundary::read_to_string(path) {
        Determination::Known(Some(t)) => t,
        Determination::Known(None) => {
            return Some(unknown(
                id,
                "transcript vanished while scanning".to_string(),
            ))
        }
        Determination::Undetermined(why) => return Some(unknown(id, why.to_string())),
    };
    let Some(last) = text.lines().rev().find(|l| !l.trim().is_empty()) else {
        return Some(unknown(id, "transcript has no entries".to_string()));
    };
    let pending = match last_pending(last) {
        Ok(p) => p,
        Err(why) => return Some(unknown(id, why)),
    };
    let pending = pending?;
    let idle = now.duration_since(mtime).map(|d| d.as_secs()).unwrap_or(0);
    if idle < STALL_SECS {
        return None;
    }
    Some(StalledSubagent {
        agent_id: id.to_string(),
        state: StallState::Stalled {
            idle_minutes: idle / 60,
            pending,
        },
    })
}

/// Ok(Some(summary)) if the entry is an assistant tool_use awaiting a result.
fn last_pending(line: &str) -> Result<Option<String>, String> {
    let v: serde_json::Value =
        serde_json::from_str(line).map_err(|e| format!("last entry is not valid JSON: {e}"))?;
    let Some(obj) = v.as_object() else {
        return Err("last entry is not a JSON object".to_string());
    };
    if obj.get("type").and_then(|t| t.as_str()) != Some("assistant") {
        return Ok(None);
    }
    let Some(content) = v.pointer("/message/content").and_then(|c| c.as_array()) else {
        return Err("assistant entry has no message.content array".to_string());
    };
    let last_use = content
        .iter()
        .rev()
        .find(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"));
    Ok(last_use.map(summarize))
}

fn summarize(block: &serde_json::Value) -> String {
    let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("?");
    let input = block.get("input");
    if name == "Bash" {
        if let Some(c) = input
            .and_then(|i| i.get("command"))
            .and_then(|c| c.as_str())
        {
            return c.to_string();
        }
    }
    let raw = input.map(|i| i.to_string()).unwrap_or_default();
    let short: String = raw.chars().take(120).collect();
    format!("{name} {short}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn use_line(cmd: &str) -> String {
        serde_json::json!({"type":"assistant","message":{"content":[
            {"type":"tool_use","id":"t","name":"Bash","input":{"command":cmd}}]}})
        .to_string()
    }
    fn result_line() -> String {
        serde_json::json!({"type":"user","message":{"content":[
            {"type":"tool_result","tool_use_id":"t","content":"ok"}]}})
        .to_string()
    }
    fn put(home: &Path, project: &Path, id: &str, body: &[u8], age: u64) {
        let d = home
            .join(".claude/projects")
            .join(slug(project))
            .join("s1/subagents");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join(format!("agent-{id}.jsonl"));
        std::fs::write(&p, body).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(age))
            .unwrap();
    }

    #[test]
    fn stalled_fresh_and_resolved() {
        let t = tempfile::TempDir::new().unwrap();
        let proj = Path::new("/x/y.z");
        put(
            t.path(),
            proj,
            "a1",
            use_line("sleep 9").as_bytes(),
            16 * 60,
        );
        put(t.path(), proj, "a2", use_line("echo").as_bytes(), 10);
        let both = format!("{}\n{}\n", use_line("echo"), result_line());
        put(t.path(), proj, "a3", both.as_bytes(), 16 * 60);
        let r = scan(t.path(), proj, SystemTime::now());
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].agent_id, "a1");
        assert!(
            matches!(&r[0].state, StallState::Stalled { idle_minutes, pending }
                if *idle_minutes >= 15 && pending == "sleep 9"),
            "{:?}",
            r[0].state
        );
    }

    #[test]
    fn unparseable_last_entry_is_unknown() {
        let t = tempfile::TempDir::new().unwrap();
        let proj = Path::new("/p");
        put(t.path(), proj, "bad", b"{not json\n", 16 * 60);
        let r = scan(t.path(), proj, SystemTime::now());
        assert_eq!(r.len(), 1);
        assert!(matches!(r[0].state, StallState::Unknown { .. }));
    }

    #[test]
    fn non_utf8_transcript_is_unknown() {
        let t = tempfile::TempDir::new().unwrap();
        let proj = Path::new("/p");
        put(t.path(), proj, "bin", &[0xff, 0xfe, 0xfd], 0);
        let r = scan(t.path(), proj, SystemTime::now());
        assert_eq!(r.len(), 1);
        assert!(matches!(r[0].state, StallState::Unknown { .. }));
    }

    #[test]
    fn missing_dirs_list_nothing() {
        let t = tempfile::TempDir::new().unwrap();
        assert!(scan(t.path(), Path::new("/nope"), SystemTime::now()).is_empty());
    }

    #[test]
    fn non_bash_tool_summary() {
        let l = serde_json::json!({"type":"assistant","message":{"content":[
            {"type":"tool_use","id":"t","name":"Edit","input":{"file_path":"/a"}}]}})
        .to_string();
        let s = last_pending(&l).unwrap().unwrap();
        assert!(s.starts_with("Edit ") && s.contains("/a"));
    }
}
