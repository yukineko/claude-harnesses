//! `ctxrot handoff-record` (PostToolUse:Read) / `ctxrot handoff` (PreToolUse:Task).
//!
//! # Scope (read before extending)
//!
//! This was scoped against a measured pattern: parallel subagents independently
//! Read-ing the same large file, paying its token cost N times. The obvious fix
//! — have a sibling subagent's `Read` short-circuit because another sibling
//! already fetched the file — is NOT achievable from a hook: the current
//! `PreToolUse`/`PostToolUse` payload carries no field correlating sibling `Task`
//! invocations (no `parent_tool_use_id`, no batch id), and even if it did, truly
//! *concurrent* siblings have no shared state to consult (neither has read
//! anything yet when both start). That case is structurally unfixable here.
//!
//! What IS fixable: the **parent** session reads a file for its own reasoning,
//! then later dispatches a `Task` whose prompt concerns the same file. Without
//! this hook, the subagent performs its own fresh `Read` — a wasted round trip,
//! and if the file is large it can walk straight into `preguard`'s own size gate
//! with no easy recourse (a subagent generally can't spawn a further sub-agent
//! to route around it). `PreToolUse` supports `hookSpecificOutput.updatedInput`
//! (only modified fields are applied; the rest of `tool_input` is untouched), so
//! this hook splices the parent's already-paid-for content into the `Task`'s
//! `prompt` before the subagent starts, making that first `Read` unnecessary.
//!
//! **This does not reduce total resident tokens system-wide.** The content still
//! has to live in the subagent's context exactly once, same as if it had `Read`
//! the file itself — token bytes are token bytes whether they arrive via a tool
//! result or via the initial prompt. What this actually saves: the wasted tool
//! round trip, and a subagent that would otherwise be denied by `preguard` with
//! nowhere to go. Do not describe this as a token-count win in docs or metrics.
//!
//! Record and lookup are keyed by `session_id` alone (the one correlating field
//! hook payloads carry), so this only ever hands content from a session back to
//! a `Task` that SAME session dispatches — never across sessions, never to a
//! sibling subagent.
//!
//! # Failure mode
//!
//! Unlike `preguard`/`toolguard`, a failure here (missing field, unreadable
//! cache, malformed JSON) has no downstream consumer that reads "nothing
//! happened" as a decision: the `Task` simply runs with its original, unmodified
//! prompt — identical to a session where this hook were never installed. This is
//! optional enrichment, not a verdict, so `run`/`record` return/no-op on any
//! failure rather than needing a `preguard`-style deny-on-panic barrier.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use serde_json::Value;

use crate::config::Config;
use harness_core::hook::HookInput;

/// Extract the primary text content from a tool response value. Mirrors
/// `toolguard::response_text`'s defensive shape-handling (a `Read` response can
/// arrive as a bare string or an object with a `text`/`content`-ish field);
/// duplicated locally (not shared) so this module has no compile-time coupling
/// to toolguard's internals.
fn response_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(response_text)
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(map) => {
            let mut parts = Vec::new();
            for key in ["text", "output", "result"] {
                if let Some(s) = map.get(key).and_then(Value::as_str) {
                    if !s.is_empty() {
                        parts.push(s.to_string());
                    }
                }
            }
            if let Some(c) = map.get("content") {
                match c {
                    Value::String(s) if !s.is_empty() => parts.push(s.clone()),
                    Value::String(_) => {}
                    other => {
                        let t = response_text(other);
                        if !t.is_empty() {
                            parts.push(t);
                        }
                    }
                }
            }
            if parts.is_empty() {
                v.to_string()
            } else {
                parts.join("\n")
            }
        }
        _ => String::new(),
    }
}

/// Path to the handoff cache (a bounded ring buffer, see `append_bounded`).
fn cache_path(cfg: &Config) -> PathBuf {
    cfg.state_dir.join("handoff.jsonl")
}

/// One cached `Read`, one JSONL line.
#[derive(serde::Serialize, serde::Deserialize)]
struct Entry {
    session: String,
    path: String,
    ts: u64,
    content: String,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Resolve a `Read` tool_input's `file_path` to an absolute, `/`-normalized
/// string — the same spelling `run` will look for inside a `Task` prompt.
fn resolve_path(input: &HookInput, raw_path: &str) -> String {
    let cwd = input.cwd_or_current();
    let expanded = crate::config::expand_tilde(raw_path);
    let resolved = if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(&expanded)
    };
    resolved.to_string_lossy().replace('\\', "/")
}

/// PostToolUse:`Read` — cache the content when it clears `handoff_min_bytes`.
/// Side-effect only (writes the cache); a hook wires this with no stdout.
pub fn record(input: &HookInput, cfg: &Config) {
    if !cfg.handoff_enabled || input.tool_name != "Read" {
        return;
    }
    let Some(ti) = input.tool_input.as_ref() else {
        return;
    };
    let Some(raw_path) = ti.get("file_path").and_then(Value::as_str) else {
        return;
    };
    let Some(resp) = input.tool_response.as_ref() else {
        return;
    };

    let content = response_text(resp);
    if (content.len() as u64) < cfg.handoff_min_bytes {
        return;
    }
    // Bound a single entry's disk/window footprint independent of the source
    // file's real size — this is a scratch cache, not a mirror of the file.
    let content: String = content
        .chars()
        .take(cfg.handoff_max_entry_bytes as usize)
        .collect();

    let entry = Entry {
        session: input.session_id.clone(),
        path: resolve_path(input, raw_path),
        ts: now_secs(),
        content,
    };
    append_bounded(cfg, &entry);
}

/// Append `entry`, then — only once the file exceeds `handoff_cache_lines`
/// lines — rewrite it keeping just the newest `handoff_cache_lines` (a bounded
/// ring buffer; this is a local scratch cache, not a durable log). Best-effort:
/// any IO error is swallowed. That is safe here specifically because a failed
/// write only means a later `Task` dispatch won't find this entry in the cache
/// — the same "nothing happened" outcome as if the read had never qualified for
/// caching in the first place (see module docstring, "Failure mode").
fn append_bounded(cfg: &Config, entry: &Entry) {
    let path = cache_path(cfg);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(line) = serde_json::to_string(entry) else {
        return;
    };
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path);
    match file {
        Ok(mut f) => {
            let _ = writeln!(f, "{line}");
        }
        Err(_) => return,
    }

    let cap = cfg.handoff_cache_lines;
    if cap == 0 {
        return;
    }
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return;
    };
    let lines: Vec<&str> = existing.lines().collect();
    if lines.len() <= cap {
        return;
    }
    let keep = lines[lines.len() - cap..].join("\n");
    let _ = std::fs::write(&path, format!("{keep}\n"));
}

/// Read all cache entries for `session`, most-recent-per-path (a later write for
/// the same path wins). Fail-soft: a missing/corrupt file yields an empty
/// vector — this is a cache, and an empty cache is a correct "nothing to offer"
/// state, not a cannot-determine (see module docstring: absence here never
/// reads as a decision downstream). Malformed individual lines are skipped
/// rather than aborting the whole read.
fn entries_for_session(cfg: &Config, session: &str) -> Vec<Entry> {
    let file = match std::fs::File::open(cache_path(cfg)) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let mut by_path: std::collections::HashMap<String, Entry> = std::collections::HashMap::new();
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(e) = serde_json::from_str::<Entry>(&line) else {
            continue;
        };
        if e.session != session {
            continue;
        }
        by_path.insert(e.path.clone(), e);
    }
    let mut v: Vec<Entry> = by_path.into_values().collect();
    v.sort_by_key(|e| e.ts);
    v
}

/// PreToolUse:`Task` — if the prompt mentions the resolved path of a file this
/// SAME session already read, splice the cached content in. Returns the
/// PARTIAL `tool_input` patch for `hookSpecificOutput.updatedInput` (only
/// `prompt` is set; every other field of the real `tool_input` is left as-is by
/// Claude Code), or `None` when nothing applies — also the correct response on
/// any internal failure (see module docstring, "Failure mode").
///
/// Matching is deliberately conservative: only the full resolved, `/`-normalized
/// path (not a bare file name) counts, so a common filename like `lib.rs`
/// mentioned in an unrelated prompt cannot trigger an unwanted content dump.
pub fn run(input: &HookInput, cfg: &Config) -> Option<Value> {
    if !cfg.handoff_enabled || input.tool_name != "Task" {
        return None;
    }
    let ti = input.tool_input.as_ref()?;
    let prompt = ti.get("prompt").and_then(Value::as_str)?;
    if prompt.is_empty() {
        return None;
    }
    let prompt_norm = prompt.replace('\\', "/");

    let cached = entries_for_session(cfg, &input.session_id);
    let mut matched: Vec<&Entry> = cached
        .iter()
        .filter(|e| prompt_norm.contains(&e.path))
        .collect();
    if matched.is_empty() {
        return None;
    }
    // Most-recently-read match first when the per-Task budget can't fit all of them.
    matched.sort_by_key(|e| std::cmp::Reverse(e.ts));

    let mut appended = String::new();
    let mut budget: i64 = cfg.handoff_max_inject_bytes as i64;
    for e in &matched {
        if budget <= 0 {
            break;
        }
        let piece: String = e.content.chars().take(budget as usize).collect();
        budget -= piece.chars().count() as i64;
        appended.push_str(&format!(
            "\n\n---\n[ctxrot handoff] `{}` は親セッションが既に読み込み済みです。\
             このファイルを Read する前に、まず以下の内容を確認してください:\n```\n{piece}\n```\n",
            e.path
        ));
    }
    if appended.is_empty() {
        return None;
    }

    crate::metrics::emit(
        cfg,
        &input.session_id,
        "handoff",
        serde_json::json!({ "files": matched.len(), "bytes": appended.len() }),
    );

    Some(serde_json::json!({ "prompt": format!("{prompt}{appended}") }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_cfg(name: &str) -> Config {
        let dir = tempfile::Builder::new()
            .prefix(&format!("ctxrot-handoff-{name}-"))
            .tempdir()
            .expect("tempdir")
            .keep();
        Config {
            state_dir: dir,
            ..Config::default()
        }
    }

    fn read_input(session: &str, cwd: &str, file_path: &str, content: &str) -> HookInput {
        HookInput {
            session_id: session.to_string(),
            cwd: cwd.to_string(),
            tool_name: "Read".to_string(),
            tool_input: Some(json!({ "file_path": file_path })),
            tool_response: Some(json!(content)),
            ..Default::default()
        }
    }

    fn task_input(session: &str, prompt: &str) -> HookInput {
        HookInput {
            session_id: session.to_string(),
            tool_name: "Task".to_string(),
            tool_input: Some(json!({ "prompt": prompt, "subagent_type": "Explore" })),
            ..Default::default()
        }
    }

    // ----- record -----

    #[test]
    fn record_ignores_small_reads() {
        let cfg = temp_cfg("small");
        let input = read_input("S1", "/proj", "/proj/a.rs", "tiny content");
        record(&input, &cfg);
        assert!(entries_for_session(&cfg, "S1").is_empty());
        let _ = std::fs::remove_dir_all(&cfg.state_dir);
    }

    #[test]
    fn record_caches_large_reads_by_resolved_path() {
        let cfg = temp_cfg("large");
        let big = "x".repeat(30_000);
        let input = read_input("S1", "/proj", "/proj/src/big.rs", &big);
        record(&input, &cfg);
        let entries = entries_for_session(&cfg, "S1");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "/proj/src/big.rs");
        assert_eq!(entries[0].content, big);
        let _ = std::fs::remove_dir_all(&cfg.state_dir);
    }

    #[test]
    fn record_ignores_non_read_tools() {
        let cfg = temp_cfg("nonread");
        let mut input = read_input("S1", "/proj", "/proj/big.rs", &"y".repeat(30_000));
        input.tool_name = "Edit".to_string();
        record(&input, &cfg);
        assert!(entries_for_session(&cfg, "S1").is_empty());
        let _ = std::fs::remove_dir_all(&cfg.state_dir);
    }

    #[test]
    fn record_relative_path_resolves_against_cwd() {
        let cfg = temp_cfg("relpath");
        let big = "z".repeat(30_000);
        let input = read_input("S1", "/proj", "src/rel.rs", &big);
        record(&input, &cfg);
        let entries = entries_for_session(&cfg, "S1");
        assert_eq!(entries[0].path, "/proj/src/rel.rs");
        let _ = std::fs::remove_dir_all(&cfg.state_dir);
    }

    #[test]
    fn record_entry_is_capped_at_max_entry_bytes() {
        let mut cfg = temp_cfg("capentry");
        cfg.handoff_max_entry_bytes = 100;
        let big = "w".repeat(30_000);
        let input = read_input("S1", "/proj", "/proj/big.rs", &big);
        record(&input, &cfg);
        let entries = entries_for_session(&cfg, "S1");
        assert_eq!(entries[0].content.chars().count(), 100);
        let _ = std::fs::remove_dir_all(&cfg.state_dir);
    }

    #[test]
    fn cache_is_a_bounded_ring_buffer() {
        let mut cfg = temp_cfg("ring");
        cfg.handoff_cache_lines = 3;
        for i in 0..6 {
            let input = read_input(
                "S1",
                "/proj",
                &format!("/proj/f{i}.rs"),
                &"a".repeat(30_000),
            );
            record(&input, &cfg);
        }
        let entries = entries_for_session(&cfg, "S1");
        assert_eq!(entries.len(), 3, "ring buffer must cap total lines");
        // The newest three (f3, f4, f5) must survive; the oldest are evicted.
        let paths: std::collections::HashSet<_> = entries.iter().map(|e| e.path.clone()).collect();
        assert!(paths.contains("/proj/f5.rs"));
        assert!(!paths.contains("/proj/f0.rs"));
        let _ = std::fs::remove_dir_all(&cfg.state_dir);
    }

    // ----- run (PreToolUse:Task) -----

    #[test]
    fn run_injects_cached_content_when_prompt_mentions_the_path() {
        let cfg = temp_cfg("inject");
        let big = "content-marker-".to_string() + &"x".repeat(30_000);
        record(&read_input("S1", "/proj", "/proj/src/big.rs", &big), &cfg);

        let task = task_input("S1", "Summarize /proj/src/big.rs for the parent session.");
        let patch = run(&task, &cfg).expect("must inject");
        let prompt = patch["prompt"].as_str().unwrap();
        assert!(prompt.contains("content-marker-"));
        assert!(prompt.contains("ctxrot handoff"));
        let _ = std::fs::remove_dir_all(&cfg.state_dir);
    }

    #[test]
    fn run_is_none_when_no_cached_path_is_mentioned() {
        let cfg = temp_cfg("nomatch");
        record(
            &read_input("S1", "/proj", "/proj/src/big.rs", &"x".repeat(30_000)),
            &cfg,
        );
        let task = task_input("S1", "Look into the auth module and report back.");
        assert!(run(&task, &cfg).is_none());
        let _ = std::fs::remove_dir_all(&cfg.state_dir);
    }

    #[test]
    fn run_is_none_for_a_different_session() {
        let cfg = temp_cfg("othersession");
        record(
            &read_input("S1", "/proj", "/proj/src/big.rs", &"x".repeat(30_000)),
            &cfg,
        );
        let task = task_input("S2", "Summarize /proj/src/big.rs please.");
        assert!(
            run(&task, &cfg).is_none(),
            "a cache entry must never cross sessions"
        );
        let _ = std::fs::remove_dir_all(&cfg.state_dir);
    }

    #[test]
    fn run_does_not_match_on_bare_filename_alone() {
        // Conservative-matching guard: mentioning just the file NAME (not the
        // full resolved path) must not trigger injection — that would risk
        // dumping unrelated content for a common name like `lib.rs`.
        let cfg = temp_cfg("basenameonly");
        record(
            &read_input("S1", "/proj", "/proj/src/lib.rs", &"x".repeat(30_000)),
            &cfg,
        );
        let task = task_input("S1", "Go look at lib.rs somewhere in the tree.");
        assert!(run(&task, &cfg).is_none());
        let _ = std::fs::remove_dir_all(&cfg.state_dir);
    }

    #[test]
    fn run_respects_the_disabled_flag() {
        let mut cfg = temp_cfg("disabled");
        cfg.handoff_enabled = false;
        record(
            &read_input("S1", "/proj", "/proj/src/big.rs", &"x".repeat(30_000)),
            &cfg,
        );
        let task = task_input("S1", "Summarize /proj/src/big.rs please.");
        assert!(run(&task, &cfg).is_none());
        let _ = std::fs::remove_dir_all(&cfg.state_dir);
    }

    #[test]
    fn run_ignores_non_task_tools() {
        let cfg = temp_cfg("nontask");
        record(
            &read_input("S1", "/proj", "/proj/src/big.rs", &"x".repeat(30_000)),
            &cfg,
        );
        let mut input = task_input("S1", "Summarize /proj/src/big.rs please.");
        input.tool_name = "Bash".to_string();
        assert!(run(&input, &cfg).is_none());
        let _ = std::fs::remove_dir_all(&cfg.state_dir);
    }

    #[test]
    fn run_caps_total_injection_at_the_configured_budget() {
        let mut cfg = temp_cfg("budget");
        cfg.handoff_max_inject_bytes = 500;
        // Use a marker that can't coincidentally collide with fixed text the hook
        // itself writes into the prompt (unlike e.g. 'x', which also appears
        // literally in the "ctxrot" brand name of the injected header).
        record(
            &read_input("S1", "/proj", "/proj/src/big.rs", &"Q".repeat(30_000)),
            &cfg,
        );
        let task = task_input("S1", "Summarize /proj/src/big.rs please.");
        let patch = run(&task, &cfg).expect("must inject");
        let prompt = patch["prompt"].as_str().unwrap();
        // The spliced piece of the cached entry must not run away with the full
        // 30_000-char content — the budget caps it at handoff_max_inject_bytes.
        let injected_q_count = prompt.matches('Q').count();
        assert_eq!(
            injected_q_count, 500,
            "injection must respect handoff_max_inject_bytes exactly, got {injected_q_count} Q's"
        );
        let _ = std::fs::remove_dir_all(&cfg.state_dir);
    }
}
