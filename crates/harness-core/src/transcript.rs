//! Streaming transcript (JSONL) reader.
//!
//! Per project policy we NEVER load a whole transcript into memory. Everything
//! here is a single forward streaming pass with O(1) or bounded memory:
//!   * `estimate_tokens` keeps only the last seen `usage` value.
//!   * `recent_turns` keeps a bounded ring buffer of the most recent turns.

use std::collections::VecDeque;
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
///
/// Returns a [`Determination`] rather than a bare `Vec` because "this transcript
/// held no user/assistant turns" and "this transcript could not be read" are
/// different facts that an empty `Vec` cannot tell apart — and every caller
/// treats an empty result as "nothing to do" (CLAUDE.md §3: never return an
/// empty collection on error). Concretely: ctxrot's rescue hook exists to save
/// the conversation before compaction, and with the old signature an unreadable
/// transcript wrote no rescue note and told nobody, which is byte-for-byte
/// indistinguishable from "there was nothing worth saving".
///
/// **Where the line sits, deliberately.** `Undetermined` means *the bytes could
/// not be obtained*: the file would not open, or a read/decode error occurred
/// mid-stream. A line whose bytes WERE obtained but which is not a turn record
/// stays a skip — including a line that is not valid JSON, exactly like a line
/// with no `message` field or a non-turn role. That is not a softening: a live
/// transcript is appended to concurrently, so its final line is routinely a
/// torn, half-written JSON record, and calling that "undetermined" would make
/// every rescue during a live session report a failure it did not have.
///
/// The case left unresolved is a *partially* readable transcript (real turns
/// plus an unreadable region). Today the read error wins and the whole answer is
/// undetermined — the restricted side — rather than returning the turns that
/// were recovered. See backlog `809cf00f`.
pub fn recent_turns(path: &str, max_turns: usize, max_chars: usize) -> Determination<Vec<Turn>> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            return Determination::undetermined(format!("cannot open transcript {path}: {e}"));
        }
    };
    let reader = BufReader::new(file);
    let mut ring: VecDeque<Turn> = VecDeque::with_capacity(max_turns + 1);

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            // The bytes could not be read or decoded. That is not "this line
            // held no turn": we do not know what it held, nor what follows it.
            Err(e) => {
                return Determination::undetermined(format!("cannot read transcript {path}: {e}"));
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        // Read successfully, but not a turn record. Same category as a line with
        // no `message` field or a non-turn role — see the fn docstring for why a
        // torn final line must not become an undetermined answer.
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
    Determination::known(ring.into_iter().collect())
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
        let Determination::Known(turns) = recent_turns(FIXTURE, 60, 1200) else {
            panic!("the checked-in fixture must be readable");
        };
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
