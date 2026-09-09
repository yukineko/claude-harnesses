//! Streaming transcript (JSONL) reader.
//!
//! Per project policy we NEVER load a whole transcript into memory. Everything
//! here is a single forward streaming pass with O(1) or bounded memory:
//!   * `estimate_tokens` keeps only the last seen `usage` value.
//!   * `recent_turns` keeps a bounded ring buffer of the most recent turns.

use std::collections::{BTreeSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader};

use serde_json::Value;

use crate::verdict::Determination;

/// Live window occupancy at the turn that wrote this usage block.
#[derive(Debug, Default, Clone, Copy)]
pub struct Usage {
    pub input: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input + self.cache_read + self.cache_creation
    }
}

fn usage_from_value(u: &Value) -> Usage {
    let g = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    Usage {
        input: g("input_tokens"),
        cache_read: g("cache_read_input_tokens"),
        cache_creation: g("cache_creation_input_tokens"),
    }
}

/// Find the usage object inside a transcript line (either `message.usage` or a
/// top-level `usage`).
fn usage_in_line(o: &Value) -> Option<Usage> {
    if let Some(u) = o.get("message").and_then(|m| m.get("usage")) {
        if u.is_object() {
            return Some(usage_from_value(u));
        }
    }
    if let Some(u) = o.get("usage") {
        if u.is_object() {
            return Some(usage_from_value(u));
        }
    }
    None
}

/// Real context occupancy from the LAST usage block in the transcript.
///
/// The last usage reflects current state and *drops after `/compact`* (cache_read
/// collapses), which a bytes-based proxy cannot capture. Streams forward keeping
/// only the most recent value.
pub fn last_usage_tokens(path: &str) -> Option<u64> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut last: Option<u64> = None;
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        // cheap pre-filter: only json-parse lines that mention usage
        if !line.contains("\"usage\"") {
            continue;
        }
        if let Ok(o) = serde_json::from_str::<Value>(&line) {
            if let Some(u) = usage_in_line(&o) {
                last = Some(u.total());
            }
        }
    }
    last
}

/// `(tokens, source)` where source is "usage" (real) or "bytes" (fallback proxy).
pub fn estimate_tokens(path: &str) -> Option<(u64, &'static str)> {
    if let Some(t) = last_usage_tokens(path) {
        return Some((t, "usage"));
    }
    let size = std::fs::metadata(path).ok()?.len();
    Some((size / 4, "bytes"))
}

/// A distilled conversational turn (role + flattened text).
#[derive(Debug, Clone)]
pub struct Turn {
    pub role: String,
    pub text: String,
}

/// Flatten a message `content` (string OR array of blocks) into plain text,
/// keeping only human-meaningful blocks (text). Tool noise is summarized, not
/// inlined, so the extraction stays small.
fn flatten_content(content: &Value, max_chars: usize) -> String {
    let mut out = String::new();
    match content {
        Value::String(s) => out.push_str(s),
        Value::Array(blocks) => {
            for b in blocks {
                match b.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = b.get("text").and_then(Value::as_str) {
                            out.push_str(t);
                            out.push('\n');
                        }
                    }
                    Some("tool_use") => {
                        let name = b.get("name").and_then(Value::as_str).unwrap_or("tool");
                        out.push_str(&format!("[tool_use: {name}]\n"));
                    }
                    Some("tool_result") => {
                        out.push_str("[tool_result]\n");
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    let out = out.trim().to_string();
    truncate_chars(&out, max_chars)
}

/// Truncate to at most `max` chars on a char boundary, appending an ellipsis marker.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max).collect();
    t.push_str(" …[truncated]");
    t
}

/// The most recent `max_turns` user/assistant turns, each capped at `max_chars`.
/// Bounded memory via a ring buffer.
pub fn recent_turns(path: &str, max_turns: usize, max_chars: usize) -> Vec<Turn> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(file);
    let mut ring: VecDeque<Turn> = VecDeque::with_capacity(max_turns + 1);

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        let o = match serde_json::from_str::<Value>(&line) {
            Ok(o) => o,
            Err(_) => continue,
        };
        let msg = match o.get("message") {
            Some(m) => m,
            None => continue,
        };
        let role = msg
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_else(|| o.get("type").and_then(Value::as_str).unwrap_or(""));
        if role != "user" && role != "assistant" {
            continue;
        }
        let content = match msg.get("content") {
            Some(c) => c,
            None => continue,
        };
        let text = flatten_content(content, max_chars);
        if text.is_empty() {
            continue;
        }
        ring.push_back(Turn {
            role: role.to_string(),
            text,
        });
        if ring.len() > max_turns {
            ring.pop_front();
        }
    }
    ring.into_iter().collect()
}

/// The tool names whose invocation means "this session wrote to a file".
///
/// `Read` / `Grep` / `Bash` are deliberately absent: reading a peer session's
/// file is *exactly* what happens when two sessions share a working tree, so an
/// implementation that merely looked for a `file_path` argument would attribute
/// the peer's file to us and re-open the bug this exists to close.
const EDIT_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// Pull the written path out of one `tool_use` block, if it is an edit.
///
/// `NotebookEdit` carries its path under `notebook_path` rather than
/// `file_path` — the same split [`crate::hook::HookInput::target`] already makes
/// for the PostToolUse payload.
fn edited_path(block: &Value) -> Option<&str> {
    if block.get("type").and_then(Value::as_str) != Some("tool_use") {
        return None;
    }
    let name = block.get("name").and_then(Value::as_str)?;
    if !EDIT_TOOLS.contains(&name) {
        return None;
    }
    let input = block.get("input")?;
    let p = input
        .get("file_path")
        .or_else(|| input.get("notebook_path"))
        .and_then(Value::as_str)?;
    if p.is_empty() {
        return None;
    }
    Some(p)
}

/// The set of files THIS session edited, read from its own transcript.
///
/// `path` is the hook payload's [`crate::hook::HookInput::transcript_path`].
///
/// # Why this exists
///
/// A git working tree carries no session identity. `git diff --name-only` in a
/// shared checkout returns every concurrent session's uncommitted edits, so a
/// gate that scans the tree presents a peer's work as yours (backlog
/// `1e44bfd9`, observed 2026-07-21 session c6a1fdbf and 2026-07-23 session
/// 143f3d21). The transcript is the one artefact that *does* carry session
/// identity, so intersecting the two is what makes attribution possible at all.
///
/// # Three-valued, and the difference is load-bearing
///
/// * `Known(set)` — the transcript was read. An EMPTY set here is a real
///   observation: this session edited nothing.
/// * `Undetermined(why)` — the transcript could not be read (empty path,
///   missing file, an IO error part-way through). We did not observe "nothing
///   was edited", we observed nothing at all.
///
/// Collapsing the second into `Known(∅)` is the fail-open this signature
/// forbids: a consumer narrows its scope to this set, so `Known(∅)` means
/// "nothing is mine" — one transcript hiccup would silently disable the
/// consuming gate entirely (CLAUDE.md 第3節).
///
/// # Decisions a reader should not have to reverse-engineer
///
/// * **Paths come back verbatim**, absolute, exactly as the transcript recorded
///   them. This function has no repo root to resolve against, and guessing one
///   (`current_dir`) mis-resolves under a `git worktree` — which is the very
///   situation 第8節 pushes every session into. Normalisation belongs to the
///   caller, which knows its root.
/// * **An unparseable LINE is skipped and the answer stays `Known`**, because
///   the file itself was readable. A transcript is appended to live, so a torn
///   final line is routine; giving up on the whole file would make this dead
///   code while looking safe. An **IO error** while reading is different — that
///   is `Undetermined`, since we can no longer say we saw the whole transcript.
/// * **Sidechain (subagent) edits COUNT as this session's.** Real transcripts
///   mark subagent turns `isSidechain: true`; this function does not filter on
///   it. A subagent edits because this session dispatched it, so the edit is
///   this session's responsibility — and the tie-break is the direction of the
///   error: including them widens the caller's scope (more gets reviewed),
///   excluding them narrows it, and 第3節 resolves an undecided case to the
///   restrictive side. If this ever needs to change, it needs its own test.
///
/// Streams the file forward one line at a time, per this module's standing
/// policy (see the module docstring); only the accumulated path set is held.
pub fn files_edited_by_session(path: &str) -> Determination<BTreeSet<String>> {
    if path.trim().is_empty() {
        return Determination::undetermined(
            "transcript_path is empty: the hook payload carried no transcript to read, so this \
             session's edit footprint was not observed",
        );
    }
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            return Determination::undetermined(format!(
                "cannot open transcript {path}: {e} — the edit footprint was not observed"
            ))
        }
    };

    let mut out = BTreeSet::new();
    for line in BufReader::new(file).lines() {
        // An IO error mid-stream means the rest of the transcript was never
        // seen. Skipping it here would hand back a SILENTLY PARTIAL set, which
        // reads downstream as "this session did not edit those files" — the
        // same collapse the signature exists to prevent.
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                return Determination::undetermined(format!(
                    "transcript {path} became unreadable part-way through: {e} — the edit \
                     footprint is partial, not empty"
                ))
            }
        };
        // A line that is not JSON is skipped; see the docstring for why this is
        // NOT the same call as the IO failure above.
        let o = match serde_json::from_str::<Value>(&line) {
            Ok(o) => o,
            Err(_) => continue,
        };
        let content = match o.get("message").and_then(|m| m.get("content")) {
            Some(Value::Array(c)) => c,
            _ => continue,
        };
        for block in content {
            if let Some(p) = edited_path(block) {
                out.insert(p.to_string());
            }
        }
    }
    Determination::Known(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "tests/fixtures/transcript.jsonl";

    #[test]
    fn reads_last_usage() {
        let t = last_usage_tokens(FIXTURE).expect("usage");
        assert_eq!(t, 1200 + 182000 + 1000);
    }

    #[test]
    fn reads_turns() {
        let turns = recent_turns(FIXTURE, 60, 1200);
        assert!(turns.len() >= 2);
        assert_eq!(turns[0].role, "user");
    }

    #[test]
    fn truncate_chars_is_cjk_safe() {
        // Counts CHARS not bytes (each kana is 3 bytes in UTF-8) and never splits
        // a multi-byte char — the harness CJK invariant.
        assert_eq!(truncate_chars("あいう", 5), "あいう"); // under the cap → unchanged
        let t = truncate_chars("あいうえお", 3);
        assert!(t.starts_with("あいう"), "{t}");
        assert!(t.ends_with("[truncated]"), "{t}");
        // Exactly at the cap keeps the whole string (boundary, no ellipsis).
        assert_eq!(truncate_chars("あいうえお", 5), "あいうえお");
    }
}
