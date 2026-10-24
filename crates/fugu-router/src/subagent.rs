//! Sub-agent model choice and outcome recording.
//!
//! * `agent-model` (PreToolUse, matcher `Agent|Task`) suggests a model for a
//!   sub-agent spawn that did not name one, learned ONLY from class
//!   [`CLASS`] episodes.
//! * `agent-record` (SubagentStop) appends one class-[`CLASS`] episode per
//!   finished sub-agent, with the model id read from the sub-agent's own
//!   transcript.
//!
//! Neither hook carries an allow/deny verdict. `agent-model`'s output is an
//! optional suggestion: when it prints nothing, the spawn keeps the caller's
//! default model, and every "nothing" path writes one stderr line saying why,
//! so silence is never presented as "the router chose the default".
//! `agent-record` never invents a value it could not read: an unreadable
//! transcript, a missing model id, or an unknown stop reason records nothing
//! (plus a stderr note) rather than a guessed episode.
//!
//! Condukt routing (`route` / `suggest` / `confidence` / `stats`' main table)
//! excludes class [`CLASS`], and this module reads only class [`CLASS`], so the
//! two populations never steer each other.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::{config, store};

/// Episode class for sub-agent outcomes.
pub const CLASS: &str = "subagent";

/// Per-part cap (chars) on the description / prompt that make up the task text.
const PART_CAP: usize = 2000;

fn cap(s: &str) -> String {
    s.chars().take(PART_CAP).collect()
}

/// The task text both hooks key on: `description + "\n" + prompt`, each part
/// capped. Spawn time and stop time must build it identically so k-NN matches.
pub fn task_text(description: &str, prompt: &str) -> String {
    format!("{}\n{}", cap(description), cap(prompt))
}

/// The pseudo touched-files context: `["agent:<type>"]`.
pub fn agent_files(agent_type: &str) -> Vec<String> {
    vec![format!("agent:{agent_type}")]
}

/// `FUGU_ROUTER_AUTO_MODEL=0` (or `false`) turns the `agent-model` hook off.
fn auto_model_off() -> bool {
    std::env::var("FUGU_ROUTER_AUTO_MODEL")
        .map(|v| {
            let v = v.trim();
            v == "0" || v.eq_ignore_ascii_case("false")
        })
        .unwrap_or(false)
}

fn note(msg: &str) {
    eprintln!("fugu-router: {msg}");
}

/// `~/.fugu-router/subagent-choice/` — one file per spawn, keyed by tool_use_id.
fn choice_dir() -> PathBuf {
    config::home_dir()
        .join(".fugu-router")
        .join("subagent-choice")
}

/// A tool_use_id usable as a single file name (no separators, no dot names).
fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

const CHOICES: [&str; 3] = ["auto", "explicit", "inherited"];

fn write_marker(tool_use_id: Option<&str>, choice: &str) {
    let Some(id) = tool_use_id.filter(|i| safe_id(i)) else {
        note(&format!(
            "agent-model: no usable tool_use_id; model choice \"{choice}\" not recorded"
        ));
        return;
    };
    let dir = choice_dir();
    let res = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(dir.join(id), choice));
    if let Err(e) = res {
        note(&format!(
            "agent-model: could not write model-choice marker for {id}: {e}"
        ));
    }
}

/// Read and remove the spawn-time marker. `None` when absent or unusable.
fn take_marker(tool_use_id: &str) -> Option<String> {
    if !safe_id(tool_use_id) {
        return None;
    }
    let path = choice_dir().join(tool_use_id);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            note(&format!(
                "agent-record: model-choice marker {} unreadable: {e}",
                path.display()
            ));
            return None;
        }
    };
    if let Err(e) = std::fs::remove_file(&path) {
        note(&format!(
            "agent-record: could not remove model-choice marker {}: {e}",
            path.display()
        ));
    }
    let v = raw.trim();
    if CHOICES.contains(&v) {
        Some(v.to_string())
    } else {
        note(&format!(
            "agent-record: model-choice marker {} has unknown value {v:?}; not recorded",
            path.display()
        ));
        None
    }
}

fn str_field<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Route a spawn from class-subagent history. `Err` carries why there is no
/// suggestion (each such path leaves the caller's default in place).
fn suggest(cfg: &config::Config, tool_input: &Value) -> Result<String, String> {
    let title = task_text(
        str_field(tool_input, "description"),
        str_field(tool_input, "prompt"),
    );
    let files = agent_files(str_field(tool_input, "subagent_type"));
    let path = cfg.store_path();
    let eps = store::load_strict(&path)
        .map_err(|e| format!("episode store {} unreadable ({e})", path.display()))?;
    let eps: Vec<store::Episode> = eps.into_iter().filter(|e| e.class == CLASS).collect();
    if eps.is_empty() {
        return Err("no class-subagent episodes recorded yet".into());
    }
    let mut rng = crate::seed_rng(eps.len());
    let d = crate::route_decision(cfg, &title, &files, CLASS, &eps, &mut rng);
    if d.basis == "prior" || d.basis == "gated" {
        return Err(format!(
            "routing basis is \"{}\" (no learned signal for this spawn)",
            d.basis
        ));
    }
    if store::model_tier(&d.worker_model) != Some(d.worker_model.as_str()) {
        return Err(format!(
            "routing returned non-tier model {:?}",
            d.worker_model
        ));
    }
    Ok(d.worker_model)
}

/// PreToolUse `Agent|Task`: print an `updatedInput` with `model` set, or nothing.
pub fn cmd_agent_model() {
    if auto_model_off() || config::disabled_env() {
        return;
    }
    let cfg = config::Config::load();
    if !cfg.enabled {
        return;
    }
    let raw = harness_core::hook::read_stdin();
    let Ok(v) = serde_json::from_str::<Value>(&raw) else {
        note("agent-model: hook input is not JSON; leaving model to the caller's default");
        return;
    };
    let tool_use_id = v.get("tool_use_id").and_then(Value::as_str);
    let Some(ti) = v.get("tool_input").filter(|t| t.is_object()) else {
        note("agent-model: no tool_input object; leaving model to the caller's default");
        return;
    };
    if ti.get("model").is_some() {
        write_marker(tool_use_id, "explicit");
        return;
    }
    match suggest(&cfg, ti) {
        Ok(model) => {
            let mut ui = ti.clone();
            ui["model"] = json!(model);
            let out = json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "updatedInput": ui,
                }
            });
            println!("{out}");
            write_marker(tool_use_id, "auto");
        }
        Err(why) => {
            note(&format!(
                "agent-model: no model suggested — {why}; leaving model to the caller's default"
            ));
            write_marker(tool_use_id, "inherited");
        }
    }
}

/// Where the sub-agent transcript lives (see module docs / hook payload).
fn transcript_path(v: &Value) -> Option<PathBuf> {
    let explicit = str_field(v, "agent_transcript_path");
    if !explicit.is_empty() {
        return Some(PathBuf::from(explicit));
    }
    let agent_id = str_field(v, "agent_id");
    let tp = str_field(v, "transcript_path");
    if agent_id.is_empty() || tp.is_empty() {
        return None;
    }
    let tp = Path::new(tp);
    let want = format!("agent-{agent_id}.jsonl");
    if tp.file_name().and_then(|n| n.to_str()) == Some(want.as_str()) {
        return Some(tp.to_path_buf());
    }
    let dir = tp.parent().unwrap_or_else(|| Path::new(""));
    Some(
        dir.join(str_field(v, "session_id"))
            .join("subagents")
            .join(want),
    )
}

fn content_text(c: &Value) -> String {
    match c {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// `(first user prompt, last assistant model id)` from a transcript.
fn scan_transcript(text: &str) -> (Option<String>, Option<String>) {
    let mut prompt = None;
    let mut model = None;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(msg) = v.get("message") else {
            continue;
        };
        match str_field(&v, "type") {
            "user" if prompt.is_none() => {
                if let Some(c) = msg.get("content") {
                    let t = content_text(c);
                    if !t.is_empty() {
                        prompt = Some(t);
                    }
                }
            }
            "assistant" => {
                let m = str_field(msg, "model");
                // `<synthetic>` marks harness-generated messages, not a model.
                if !m.is_empty() && m != "<synthetic>" {
                    model = Some(m.to_string());
                }
            }
            _ => {}
        }
    }
    (prompt, model)
}

/// SubagentStop: append one class-subagent episode, or nothing (with a note).
pub fn cmd_agent_record() {
    if config::disabled_env() {
        return;
    }
    let cfg = config::Config::load();
    if !cfg.enabled {
        return;
    }
    let raw = harness_core::hook::read_stdin();
    let Ok(v) = serde_json::from_str::<Value>(&raw) else {
        note("agent-record: hook input is not JSON; nothing recorded");
        return;
    };
    let Some(tpath) = transcript_path(&v) else {
        note("agent-record: cannot locate the sub-agent transcript (no agent_id/transcript_path); nothing recorded");
        return;
    };

    // Meta first, so the spawn-time marker is consumed on every path.
    let meta: Option<Value> = std::fs::read_to_string(tpath.with_extension("meta.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());
    let model_choice = meta
        .as_ref()
        .map(|m| str_field(m, "toolUseId"))
        .filter(|id| !id.is_empty())
        .and_then(take_marker);

    let pass = match v.get("stop_reason").and_then(Value::as_str) {
        Some("completed") => true,
        Some("failed") | Some("timeout") => false,
        Some("user_interrupt") => return,
        other => {
            note(&format!(
                "agent-record: stop_reason {other:?} cannot be mapped to pass/fail; nothing recorded"
            ));
            return;
        }
    };

    let text = match std::fs::read_to_string(&tpath) {
        Ok(t) => t,
        Err(e) => {
            note(&format!(
                "agent-record: sub-agent transcript {} unreadable ({e}); nothing recorded",
                tpath.display()
            ));
            return;
        }
    };
    let (prompt, model) = scan_transcript(&text);
    let Some(model) = model else {
        note(&format!(
            "agent-record: no assistant model id in {}; nothing recorded",
            tpath.display()
        ));
        return;
    };

    let description = meta
        .as_ref()
        .map(|m| str_field(m, "description"))
        .unwrap_or("");
    let mut agent_type = str_field(&v, "agent_type");
    if agent_type.is_empty() {
        agent_type = meta
            .as_ref()
            .map(|m| str_field(m, "agentType"))
            .unwrap_or("");
    }
    let mut ep = store::Episode {
        ts: store::now_secs(),
        title: task_text(description, prompt.as_deref().unwrap_or("")),
        touched_files: agent_files(agent_type),
        class: CLASS.to_string(),
        model,
        role: "worker".to_string(),
        pass,
        model_choice,
        ..Default::default()
    };
    store::normalize_model(&mut ep);
    if let Err(e) = store::append(&cfg.store_path(), &ep) {
        note(&format!("agent-record: appending episode failed: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_takes_first_prompt_and_last_model() {
        let t = concat!(
            r#"{"type":"user","message":{"content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"claude-haiku-4-5"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"later"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"claude-opus-5-5"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"<synthetic>"}}"#,
            "\n",
        );
        let (p, m) = scan_transcript(t);
        assert_eq!(p.as_deref(), Some("a\nb"));
        assert_eq!(m.as_deref(), Some("claude-opus-5-5"));
    }

    #[test]
    fn scan_without_model_is_none() {
        let (_, m) = scan_transcript(r#"{"type":"user","message":{"content":"x"}}"#);
        assert_eq!(m, None);
    }

    #[test]
    fn transcript_path_prefers_explicit_then_self_then_derived() {
        let v = json!({"agent_transcript_path": "/x/a.jsonl", "agent_id": "q"});
        assert_eq!(transcript_path(&v), Some(PathBuf::from("/x/a.jsonl")));
        let v = json!({"transcript_path": "/p/agent-q.jsonl", "agent_id": "q"});
        assert_eq!(transcript_path(&v), Some(PathBuf::from("/p/agent-q.jsonl")));
        let v = json!({"transcript_path": "/p/S.jsonl", "agent_id": "q", "session_id": "S"});
        assert_eq!(
            transcript_path(&v),
            Some(PathBuf::from("/p/S/subagents/agent-q.jsonl"))
        );
        assert_eq!(
            transcript_path(&json!({"transcript_path": "/p/S.jsonl"})),
            None
        );
    }

    #[test]
    fn safe_id_rejects_path_tricks() {
        assert!(safe_id("toolu_01AbC-9"));
        assert!(!safe_id("../x"));
        assert!(!safe_id("a/b"));
        assert!(!safe_id(""));
    }

    #[test]
    fn task_text_caps_each_part() {
        let long = "x".repeat(PART_CAP + 50);
        let t = task_text(&long, "p");
        assert_eq!(t.chars().count(), PART_CAP + 2);
        assert!(t.ends_with("\np"));
    }
}
