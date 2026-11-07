//! Subagent registry (RECORD) and per-agent liveness check (CHECK), backlog
//! afd18dc0.
//!
//! RECORD: `overwatch presence` (the hook) is also routed PostToolUse
//! `Agent|Task` and `SubagentStop`. Each such payload is recorded in
//! `subagents.json` beside `presence.json` (same store, keyed by the main
//! worktree root): `session_id -> agent_id -> AgentRecord`.
//!
//! * PostToolUse Agent/Task: `tool_response.agentId` is the launched agent;
//!   a top-level `agent_id` in the payload is the LAUNCHER (nested launch),
//!   absent for a launch from the main thread. `tool_response.status` is
//!   `async_launched` for a background launch and `completed` for a sync one
//!   (whose result is in `tool_response.content`, i.e. already delivered).
//! * SubagentStop: `agent_id`, `agent_transcript_path`,
//!   `last_assistant_message`. A stop is NOT a delivered result (an agent can
//!   end its turn with a text report and no SubagentHandback, observed for
//!   a56b4006f1b96dc82); it only means "the turn ended".
//!
//! CHECK: `overwatch subagents --session <id> --stall-secs N [--json]`
//! classifies every recorded agent of that session (nested included, other
//! sessions excluded). See [`State`] for the states and [`classify`] for the
//! rules. Exit 0 only when every agent is `done` or `progressing`.
//!
//! Fail-closed (CLAUDE.md section 3): an unreadable/corrupt registry, a
//! session with no record, an unreadable/unparseable transcript, an
//! unparseable entry timestamp, or a failure to persist harvested data are
//! never clean. A registry that cannot be loaded is never overwritten.
//!
//! Durability (extension i): the check writes back what it observed and must
//! not lose — `delivered` once a SubagentHandback success was seen, and the
//! transcript's last assistant text of an agent that ended undelivered
//! (`harvested_final_text`) — so a later run with the transcript deleted
//! still reports `ended_undelivered` with its `final_text`.
//!
//! Retention: sessions whose last recorded event is older than
//! [`PRUNE_AFTER_SECS`] are dropped on the next hook write; a check of such a
//! session then reports "no record" (not clean), never clean.

use crate::lock::LeaseLock;
use crate::store;
use anyhow::{bail, Result};
use harness_core::verdict::Determination;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Tool names whose PostToolUse is an agent launch.
pub const AGENT_TOOLS: [&str; 2] = ["Agent", "Task"];

/// Sessions not touched by any hook for this long are pruned.
pub const PRUNE_AFTER_SECS: i64 = 30 * 86_400;

/// This many trailing tool calls with identical (tool, input, result) is a
/// loop, classified `stalled`.
pub const LOOP_REPEATS: usize = 4;

/// The tool whose success means the agent's report reached its caller.
const HANDBACK_TOOL: &str = "SubagentHandback";

/// One recorded agent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRecord {
    pub agent_id: String,
    /// Agent that launched this one (payload `agent_id` of a nested launch);
    /// `None` = launched from the main thread (or launch never recorded).
    #[serde(default)]
    pub launcher: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub subagent_type: String,
    /// `tool_response.status` of the launch (`async_launched` / `completed`);
    /// empty when only a SubagentStop was recorded.
    #[serde(default)]
    pub launch_status: String,
    #[serde(default)]
    pub launched_at: Option<i64>,
    #[serde(default)]
    pub launch_tool_use_id: String,
    #[serde(default)]
    pub output_file: String,
    /// Agent transcript (derived from the session transcript at launch,
    /// overridden by SubagentStop's `agent_transcript_path`).
    #[serde(default)]
    pub transcript_path: String,
    /// Number of SubagentStop events seen (an agent resumed by messages stops
    /// several times).
    #[serde(default)]
    pub stops: u32,
    #[serde(default)]
    pub last_stop_at: Option<i64>,
    /// `last_assistant_message` of the most recent SubagentStop payload.
    #[serde(default)]
    pub stop_last_assistant_message: Option<String>,
    /// Transcript's last assistant text, harvested by a check that saw the
    /// agent ended without delivering. Durable across transcript deletion.
    #[serde(default)]
    pub harvested_final_text: Option<String>,
    /// A SubagentHandback success was observed by some check (durable).
    #[serde(default)]
    pub delivered: bool,
}

/// All agents of one session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_id: String,
    /// The session's main transcript (where status queries to top-level
    /// agents are recorded).
    #[serde(default)]
    pub parent_transcript_path: String,
    /// Unix seconds of the most recent recorded hook event.
    pub last_event: i64,
    #[serde(default)]
    pub agents: BTreeMap<String, AgentRecord>,
}

/// session_id -> record.
pub type SubagentRegistry = BTreeMap<String, SessionRecord>;

/// Whether `payload` is an event this registry records.
pub fn applies(payload: &Value) -> bool {
    let event = str_field(payload, "hook_event_name").unwrap_or("");
    match event {
        "SubagentStop" => true,
        "PostToolUse" => AGENT_TOOLS.contains(&str_field(payload, "tool_name").unwrap_or("")),
        _ => false,
    }
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// `<dir>/<session>.jsonl` -> `<dir>/<session>/subagents/agent-<id>.jsonl`.
fn derive_transcript(parent: &str, agent: &str) -> Option<String> {
    let stem = parent.strip_suffix(".jsonl")?;
    Some(
        Path::new(stem)
            .join("subagents")
            .join(format!("agent-{agent}.jsonl"))
            .to_string_lossy()
            .into_owned(),
    )
}

/// Registry update for one hook payload. Events this registry does not record
/// are a no-op; a recordable event missing its identifying fields is an error
/// (silently skipping it would leave an agent unrecorded, i.e. invisible).
pub fn apply_event(reg: &mut SubagentRegistry, payload: &Value, now: i64) -> Result<()> {
    if !applies(payload) {
        return Ok(());
    }
    let Some(sid) = str_field(payload, "session_id") else {
        bail!("subagent hook payload has no session_id");
    };
    let event = str_field(payload, "hook_event_name").unwrap_or("");
    let parent = str_field(payload, "transcript_path").unwrap_or("");
    let sess = reg.entry(sid.to_string()).or_default();
    sess.session_id = sid.to_string();
    if !parent.is_empty() {
        sess.parent_transcript_path = parent.to_string();
    }
    sess.last_event = now;

    if event == "SubagentStop" {
        let Some(aid) = str_field(payload, "agent_id") else {
            bail!("SubagentStop payload has no agent_id");
        };
        let rec = sess.agents.entry(aid.to_string()).or_default();
        rec.agent_id = aid.to_string();
        if let Some(t) = str_field(payload, "agent_transcript_path") {
            rec.transcript_path = t.to_string();
        } else if rec.transcript_path.is_empty() {
            if let Some(t) = derive_transcript(parent, aid) {
                rec.transcript_path = t;
            }
        }
        if rec.subagent_type.is_empty() {
            if let Some(t) = str_field(payload, "agent_type") {
                rec.subagent_type = t.to_string();
            }
        }
        rec.stops = rec.stops.saturating_add(1);
        rec.last_stop_at = Some(now);
        if let Some(m) = payload
            .get("last_assistant_message")
            .and_then(Value::as_str)
        {
            rec.stop_last_assistant_message = Some(m.to_string());
        }
    } else {
        let resp = payload.get("tool_response");
        let Some(aid) = resp
            .and_then(|r| r.get("agentId"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            bail!(
                "PostToolUse {} payload has no tool_response.agentId; the launched \
                 agent cannot be recorded",
                str_field(payload, "tool_name").unwrap_or("?")
            );
        };
        let rec = sess.agents.entry(aid.to_string()).or_default();
        rec.agent_id = aid.to_string();
        rec.launcher = str_field(payload, "agent_id").map(str::to_string);
        let input = payload.get("tool_input");
        if let Some(d) = input
            .and_then(|i| str_field(i, "description"))
            .or_else(|| resp.and_then(|r| str_field(r, "description")))
        {
            rec.description = d.to_string();
        }
        if let Some(t) = input
            .and_then(|i| str_field(i, "subagent_type"))
            .or_else(|| resp.and_then(|r| str_field(r, "agentType")))
        {
            rec.subagent_type = t.to_string();
        }
        rec.launch_status = resp
            .and_then(|r| str_field(r, "status"))
            .unwrap_or("")
            .to_string();
        rec.launched_at = Some(now);
        if let Some(t) = str_field(payload, "tool_use_id") {
            rec.launch_tool_use_id = t.to_string();
        }
        if let Some(o) = resp.and_then(|r| str_field(r, "outputFile")) {
            rec.output_file = o.to_string();
        }
        if rec.transcript_path.is_empty() {
            if let Some(t) = derive_transcript(parent, aid) {
                rec.transcript_path = t;
            }
        }
    }
    reg.retain(|_, s| now - s.last_event <= PRUNE_AFTER_SECS);
    Ok(())
}

/// Path to the subagents.json registry, beside `presence.json` (same store,
/// keyed by the main worktree root).
pub fn subagents_path(cwd: &Path) -> Result<PathBuf> {
    Ok(store::storage_root(cwd)?.join("subagents.json"))
}

/// Load `subagents.json`. Absent => `Ok(None)`; unreadable or corrupt =>
/// `Err` (never an empty registry).
pub fn load(cwd: &Path) -> Result<Option<SubagentRegistry>> {
    let path = subagents_path(cwd)?;
    match harness_core::boundary::read_to_string(&path) {
        Determination::Known(None) => Ok(None),
        Determination::Known(Some(txt)) => serde_json::from_str::<SubagentRegistry>(&txt)
            .map(Some)
            .map_err(|e| {
                anyhow::anyhow!(
                    "subagents.json present but corrupt at {}: {e}",
                    path.display()
                )
            }),
        Determination::Undetermined(why) => bail!(
            "subagents.json could not be read at {}: {why}",
            path.display()
        ),
    }
}

fn save(cwd: &Path, reg: &SubagentRegistry) -> Result<()> {
    let path = subagents_path(cwd)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(reg)?;
    let tmp = store::unique_tmp(&path, "json");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Record one payload. The caller holds the store lock.
pub fn record_locked(cwd: &Path, payload: &Value, now: i64) -> Result<()> {
    let mut reg = load(cwd)?.unwrap_or_default();
    apply_event(&mut reg, payload, now)?;
    save(cwd, &reg)
}

// ---------------------------------------------------------------- CHECK side

/// Per-agent verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Delivered its result: a SubagentHandback tool_use whose tool_result is
    /// success, or a sync launch whose PostToolUse status was `completed`.
    Done,
    /// Its latest tool result (or, before any, its first entry) is within the
    /// stall window, no loop, no unanswered query past the window.
    Progressing,
    /// Repeated identical tool calls, or a status query (SendMessage to it
    /// from its parent/launcher) unanswered for longer than the window.
    Stalled,
    /// No tool result for longer than the window and no overdue query: the
    /// parent must query it now (SendMessage) — not clean.
    QueryRequired,
    /// Its turn ended (SubagentStop after its last activity) without a
    /// delivered result. `final_text` carries its last assistant text.
    EndedUndelivered,
    /// Checked and still unknown (no/unreadable/unparseable transcript, ...).
    Undetermined,
}

impl State {
    pub fn is_clean(self) -> bool {
        matches!(self, State::Done | State::Progressing)
    }
}

/// One row of the check output. Field names are a contract (condukt reads
/// `agent_id`, `state`, `final_text`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentReport {
    pub agent_id: String,
    pub state: State,
    pub reason: String,
    pub launcher: Option<String>,
    pub description: String,
    pub subagent_type: String,
    /// Seconds since the latest tool result (or first entry), when known.
    pub idle_secs: Option<i64>,
    /// Last assistant text of an `ended_undelivered` agent; null otherwise.
    pub final_text: Option<String>,
    pub transcript_path: String,
}

/// Whole check output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckReport {
    pub clean: bool,
    pub session: String,
    pub stall_secs: u64,
    pub agents: Vec<AgentReport>,
    /// Set when the check itself could not be completed (registry
    /// unreadable, no record for the session, lock/persist failure).
    pub error: Option<String>,
}

/// What a transcript says, from its entries' own timestamps.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Facts {
    pub handback_ok: bool,
    pub first_ts: Option<i64>,
    pub last_result_ts: Option<i64>,
    /// Latest assistant entry or tool result.
    pub last_activity_ts: Option<i64>,
    pub last_assistant_text: Option<String>,
    /// `(tool, input, result)` keys of completed tool calls, in order.
    pub calls: Vec<String>,
    /// Timestamps of SendMessage tool_uses, keyed by their `to`.
    pub sent: BTreeMap<String, Vec<i64>>,
}

/// Parse RFC 3339 (`YYYY-MM-DDTHH:MM:SS[.frac](Z|±HH:MM)`) to unix seconds.
pub fn parse_ts(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    if b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = s.get(r)?;
        if part.bytes().all(|c| c.is_ascii_digit()) {
            part.parse().ok()
        } else {
            None
        }
    };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, se) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let mut i = 19;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == start {
            return None;
        }
    }
    let offset = match b.get(i) {
        Some(b'Z' | b'z') if i + 1 == b.len() => 0,
        Some(sign @ (b'+' | b'-')) if i + 6 == b.len() && b[i + 3] == b':' => {
            let oh = num(i + 1..i + 3)?;
            let om = num(i + 4..i + 6)?;
            let o = oh * 3600 + om * 60;
            if *sign == b'+' {
                o
            } else {
                -o
            }
        }
        _ => return None,
    };
    // days_from_civil (Howard Hinnant).
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + se - offset)
}

fn block_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A SubagentHandback tool_result counts as success only when its text is a
/// JSON object with `"success": true` and the block is not an error.
fn handback_success(block: &Value) -> bool {
    if block.get("is_error").and_then(Value::as_bool) == Some(true) {
        return false;
    }
    let text = block_text(block.get("content").unwrap_or(&Value::Null));
    serde_json::from_str::<Value>(text.trim())
        .ok()
        .and_then(|v| v.get("success").and_then(Value::as_bool))
        == Some(true)
}

/// Parse a transcript into [`Facts`]. Hook `attachment` entries and other
/// non-message entries are ignored (they say nothing about progress). A
/// trailing line without its newline that does not parse is a concurrent
/// partial write and is skipped; any other unparseable line, or a message
/// entry without a parseable timestamp, is an error.
pub fn parse_transcript(text: &str) -> Result<Facts, String> {
    let mut f = Facts::default();
    let mut uses: BTreeMap<String, (String, String)> = BTreeMap::new();
    let complete = text.ends_with('\n');
    let lines: Vec<&str> = text.lines().collect();
    let n = lines.len();
    for (i, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) if i + 1 == n && !complete => continue,
            Err(e) => return Err(format!("transcript line {} is not valid JSON: {e}", i + 1)),
        };
        let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
        if kind != "assistant" && kind != "user" {
            continue;
        }
        let ts = v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_ts)
            .ok_or_else(|| format!("transcript line {} has no parseable timestamp", i + 1))?;
        if f.first_ts.is_none() {
            f.first_ts = Some(ts);
        }
        let content = v.pointer("/message/content");
        let blocks: &[Value] = content.and_then(Value::as_array).map_or(&[], |a| a);
        if kind == "assistant" {
            f.last_activity_ts = Some(f.last_activity_ts.map_or(ts, |t| t.max(ts)));
            let mut texts = vec![];
            for b in blocks {
                match b.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = b.get("text").and_then(Value::as_str) {
                            if !t.trim().is_empty() {
                                texts.push(t);
                            }
                        }
                    }
                    Some("tool_use") => {
                        let id = b.get("id").and_then(Value::as_str).unwrap_or("");
                        let name = b.get("name").and_then(Value::as_str).unwrap_or("");
                        let input = b.get("input").cloned().unwrap_or(Value::Null);
                        if name == "SendMessage" {
                            if let Some(to) = input.get("to").and_then(Value::as_str) {
                                f.sent.entry(to.to_string()).or_default().push(ts);
                            }
                        }
                        uses.insert(id.to_string(), (name.to_string(), input.to_string()));
                    }
                    _ => {}
                }
            }
            if !texts.is_empty() {
                f.last_assistant_text = Some(texts.join("\n"));
            }
        } else {
            for b in blocks {
                if b.get("type").and_then(Value::as_str) != Some("tool_result") {
                    continue;
                }
                f.last_result_ts = Some(f.last_result_ts.map_or(ts, |t| t.max(ts)));
                f.last_activity_ts = Some(f.last_activity_ts.map_or(ts, |t| t.max(ts)));
                let id = b.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
                let (name, input) = uses.get(id).cloned().unwrap_or_default();
                if name == HANDBACK_TOOL && handback_success(b) {
                    f.handback_ok = true;
                }
                let result = block_text(b.get("content").unwrap_or(&Value::Null));
                f.calls.push(format!("{name}\u{1f}{input}\u{1f}{result}"));
            }
        }
    }
    Ok(f)
}

fn looping(calls: &[String]) -> bool {
    calls.len() >= LOOP_REPEATS && {
        let tail = &calls[calls.len() - LOOP_REPEATS..];
        tail.iter().all(|c| c == &tail[0])
    }
}

/// Outcome of classifying one agent, plus what to persist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classified {
    pub state: State,
    pub reason: String,
    pub idle_secs: Option<i64>,
    pub final_text: Option<String>,
    /// Newly observed delivery to persist.
    pub persist_delivered: bool,
    /// Newly harvested final text to persist.
    pub persist_final_text: Option<String>,
}

fn verdict(state: State, reason: impl Into<String>) -> Classified {
    Classified {
        state,
        reason: reason.into(),
        idle_secs: None,
        final_text: None,
        persist_delivered: false,
        persist_final_text: None,
    }
}

/// Classify one agent.
///
/// * `transcript`: the agent transcript read (`Known(None)` = absent).
/// * `queries`: timestamps of status queries (SendMessage `to` this agent)
///   from its parent/launcher, or `Err` when they could not be read.
///
/// Rules, in order:
/// 1. durable `delivered`, sync launch `completed`, or a SubagentHandback
///    success in the transcript => `done`;
/// 2. transcript absent: SubagentStop recorded => `ended_undelivered` with the
///    harvested text or the stop payload's `last_assistant_message`;
///    otherwise `undetermined`. Unreadable/unparseable => `undetermined`;
/// 3. a SubagentStop at or after the last assistant entry / tool result =>
///    `ended_undelivered` (final text = transcript's last assistant text);
/// 4. last [`LOOP_REPEATS`] tool calls identical => `stalled`;
/// 5. queries unreadable => `undetermined`;
/// 6. a query newer than the agent's last activity and older than
///    `stall_secs` => `stalled`;
/// 7. idle (since last tool result, else first entry) <= `stall_secs` =>
///    `progressing`, else `query_required`; no entry at all => `undetermined`.
pub fn classify(
    rec: &AgentRecord,
    transcript: &Determination<Option<String>>,
    queries: &Result<Vec<i64>, String>,
    now: i64,
    stall_secs: i64,
) -> Classified {
    if rec.delivered {
        return verdict(State::Done, "delivery recorded (SubagentHandback success)");
    }
    if rec.launch_status == "completed" {
        return verdict(State::Done, "sync launch completed with its result");
    }
    let text = match transcript {
        Determination::Known(Some(t)) => t,
        Determination::Known(None) => {
            if rec.stops > 0 {
                let mut c = verdict(
                    State::EndedUndelivered,
                    "SubagentStop recorded, no delivery recorded, transcript absent",
                );
                c.final_text = rec
                    .harvested_final_text
                    .clone()
                    .or_else(|| rec.stop_last_assistant_message.clone());
                return c;
            }
            return verdict(
                State::Undetermined,
                format!(
                    "recorded agent has no transcript at {:?}",
                    rec.transcript_path
                ),
            );
        }
        Determination::Undetermined(why) => {
            return verdict(State::Undetermined, format!("transcript unreadable: {why}"))
        }
    };
    let facts = match parse_transcript(text) {
        Ok(f) => f,
        Err(why) => {
            return verdict(
                State::Undetermined,
                format!("transcript unparseable: {why}"),
            )
        }
    };
    if facts.handback_ok {
        let mut c = verdict(State::Done, "SubagentHandback tool_result success");
        c.persist_delivered = true;
        return c;
    }
    let ended = rec.stops > 0
        && rec
            .last_stop_at
            .is_some_and(|s| facts.last_activity_ts.is_none_or(|a| s >= a));
    if ended {
        let mut c = verdict(
            State::EndedUndelivered,
            "turn ended (SubagentStop) without a SubagentHandback success",
        );
        c.persist_final_text = facts.last_assistant_text.clone();
        c.final_text = facts
            .last_assistant_text
            .clone()
            .or_else(|| rec.harvested_final_text.clone())
            .or_else(|| rec.stop_last_assistant_message.clone());
        return c;
    }
    let reference = facts.last_result_ts.or(facts.first_ts);
    let idle = reference.map(|r| now - r);
    if looping(&facts.calls) {
        let mut c = verdict(
            State::Stalled,
            format!("last {LOOP_REPEATS} tool calls are identical with identical results"),
        );
        c.idle_secs = idle;
        return c;
    }
    let queries = match queries {
        Ok(q) => q,
        Err(why) => {
            let mut c = verdict(
                State::Undetermined,
                format!("status queries to this agent could not be read: {why}"),
            );
            c.idle_secs = idle;
            return c;
        }
    };
    let activity = facts.last_activity_ts.unwrap_or(i64::MIN);
    let unanswered = queries.iter().copied().filter(|q| *q > activity).min();
    if let Some(q) = unanswered {
        if now - q > stall_secs {
            let mut c = verdict(
                State::Stalled,
                format!("status query sent {}s ago is unanswered", now - q),
            );
            c.idle_secs = idle;
            return c;
        }
    }
    let Some(idle_v) = idle else {
        return verdict(State::Undetermined, "transcript has no message entries");
    };
    let mut c = if idle_v <= stall_secs {
        verdict(
            State::Progressing,
            if facts.last_result_ts.is_some() {
                "latest tool result within the stall window"
            } else {
                "started within the stall window, no tool result yet"
            },
        )
    } else {
        verdict(
            State::QueryRequired,
            format!(
                "no tool result for {idle_v}s (> {stall_secs}s); query it (SendMessage {})",
                rec.agent_id
            ),
        )
    };
    c.idle_secs = idle;
    c
}

/// SendMessage timestamps addressed to `agent` in the transcript at `path`.
/// Absent file => none; unreadable/unparseable => `Err`.
fn queries_in(
    cache: &mut BTreeMap<String, Result<Facts, String>>,
    path: &str,
    agent: &str,
) -> Result<Vec<i64>, String> {
    let facts =
        cache.entry(path.to_string()).or_insert_with(
            || match harness_core::boundary::read_to_string(Path::new(path)) {
                Determination::Known(None) => Ok(Facts::default()),
                Determination::Known(Some(t)) => parse_transcript(&t),
                Determination::Undetermined(why) => Err(why.to_string()),
            },
        );
    match facts {
        Ok(f) => Ok(f.sent.get(agent).cloned().unwrap_or_default()),
        Err(why) => Err(format!("{path}: {why}")),
    }
}

/// Run the check against the registry for `cwd`. Holds the store lock for
/// the read-classify-persist cycle.
pub fn check(cwd: &Path, session: &str, stall_secs: u64, now: i64) -> CheckReport {
    let mut report = CheckReport {
        clean: false,
        session: session.to_string(),
        stall_secs,
        agents: vec![],
        error: None,
    };
    let _lock = match LeaseLock::acquire_strict(cwd) {
        Ok(l) => l,
        Err(e) => {
            report.error = Some(format!("store lock not acquired: {e}"));
            return report;
        }
    };
    let mut reg = match load(cwd) {
        Ok(Some(r)) => r,
        Ok(None) => {
            report.error = Some(
                "no subagent registry (subagents.json absent): nothing was ever recorded, \
                 so no agent can be vouched for"
                    .to_string(),
            );
            return report;
        }
        Err(e) => {
            report.error = Some(format!("{e:#}"));
            return report;
        }
    };
    let Some(sess) = reg.get_mut(session) else {
        report.error = Some(format!("no subagent recorded for session {session}"));
        return report;
    };
    let stall = i64::try_from(stall_secs).unwrap_or(i64::MAX);
    let mut cache: BTreeMap<String, Result<Facts, String>> = BTreeMap::new();
    let parent_path = sess.parent_transcript_path.clone();
    let mut dirty = false;
    let snapshot: Vec<AgentRecord> = sess.agents.values().cloned().collect();
    for rec in &snapshot {
        let transcript = if rec.transcript_path.is_empty() {
            Determination::Known(None)
        } else {
            harness_core::boundary::read_to_string(Path::new(&rec.transcript_path))
        };
        let queries = collect_queries(&mut cache, &parent_path, rec);
        let c = classify(rec, &transcript, &queries, now, stall);
        if let Some(r) = sess.agents.get_mut(&rec.agent_id) {
            if c.persist_delivered && !r.delivered {
                r.delivered = true;
                dirty = true;
            }
            if let Some(t) = &c.persist_final_text {
                if r.harvested_final_text.as_ref() != Some(t) {
                    r.harvested_final_text = Some(t.clone());
                    dirty = true;
                }
            }
        }
        report.agents.push(AgentReport {
            agent_id: rec.agent_id.clone(),
            state: c.state,
            reason: c.reason,
            launcher: rec.launcher.clone(),
            description: rec.description.clone(),
            subagent_type: rec.subagent_type.clone(),
            idle_secs: c.idle_secs,
            final_text: c.final_text,
            transcript_path: rec.transcript_path.clone(),
        });
    }
    report.clean = !report.agents.is_empty() && report.agents.iter().all(|a| a.state.is_clean());
    if report.agents.is_empty() {
        report.error = Some(format!("session {session} has no agent records"));
    }
    if dirty {
        if let Err(e) = save(cwd, &reg) {
            report.clean = false;
            report.error = Some(format!(
                "observed delivery / harvested final text could not be persisted: {e:#}"
            ));
        }
    }
    report
}

/// Queries from the session transcript (top-level agents) and, for a nested
/// agent, from its launcher's transcript (sibling file in the same
/// `subagents/` directory).
fn collect_queries(
    cache: &mut BTreeMap<String, Result<Facts, String>>,
    parent_path: &str,
    rec: &AgentRecord,
) -> Result<Vec<i64>, String> {
    let mut paths: Vec<String> = vec![];
    match &rec.launcher {
        None => {
            if parent_path.is_empty() {
                return Err("session transcript path was never recorded".to_string());
            }
            paths.push(parent_path.to_string());
        }
        Some(l) => {
            let dir = PathBuf::from(&rec.transcript_path);
            match dir.parent() {
                Some(d) if !rec.transcript_path.is_empty() => paths.push(
                    d.join(format!("agent-{l}.jsonl"))
                        .to_string_lossy()
                        .into_owned(),
                ),
                _ => return Err(format!("launcher {l} transcript path not derivable")),
            }
            if !parent_path.is_empty() {
                paths.push(parent_path.to_string());
            }
        }
    }
    let mut out = vec![];
    for p in paths {
        out.extend(queries_in(cache, &p, &rec.agent_id)?);
    }
    Ok(out)
}

/// `overwatch subagents`: print the report, return the exit code (0 clean,
/// 1 otherwise — never 2, which clap uses for usage errors).
pub fn run_cli(session: &str, stall_secs: u64, json: bool) -> i32 {
    let report = match std::env::current_dir() {
        Ok(cwd) => check(&cwd, session, stall_secs, store::now()),
        Err(e) => CheckReport {
            clean: false,
            session: session.to_string(),
            stall_secs,
            agents: vec![],
            error: Some(format!("cannot resolve cwd: {e}")),
        },
    };
    if json {
        match serde_json::to_string_pretty(&report) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                println!("{{\"clean\":false,\"agents\":[],\"error\":\"serialize failed\"}}");
                eprintln!("overwatch subagents: serialize failed: {e}");
                return 1;
            }
        }
    } else {
        println!(
            "subagents of session {} (stall window {}s): {}",
            report.session,
            report.stall_secs,
            if report.clean { "clean" } else { "NOT clean" }
        );
        if let Some(e) = &report.error {
            println!("  error: {e}");
        }
        for a in &report.agents {
            let state = serde_json::to_value(a.state)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| format!("{:?}", a.state));
            println!("  {} {} — {}", a.agent_id, state, a.reason);
        }
    }
    i32::from(!report.clean)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_ts_utc_and_offset() {
        assert_eq!(parse_ts("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_ts("1970-01-01T00:00:01.999Z"), Some(1));
        assert_eq!(parse_ts("2026-11-07T20:29:08.000Z"), Some(1_794_083_348));
        assert_eq!(parse_ts("1970-01-01T09:00:00+09:00"), Some(0));
        assert_eq!(parse_ts("not a time"), None);
        assert_eq!(parse_ts("2026-13-07T20:29:08Z"), None);
        assert_eq!(parse_ts("2026-11-07T20:29:08"), None);
    }

    #[test]
    fn launch_without_agent_id_is_an_error() {
        let mut reg = SubagentRegistry::new();
        let p = json!({"session_id":"s","hook_event_name":"PostToolUse","tool_name":"Agent",
            "tool_response":{"status":"async_launched"}});
        assert!(apply_event(&mut reg, &p, 1).is_err());
    }

    #[test]
    fn non_agent_events_are_ignored() {
        let mut reg = SubagentRegistry::new();
        let p = json!({"session_id":"s","hook_event_name":"PostToolUse","tool_name":"Edit"});
        apply_event(&mut reg, &p, 1).unwrap();
        assert!(reg.is_empty());
    }

    #[test]
    fn nested_launch_records_launcher_and_derived_transcript() {
        let mut reg = SubagentRegistry::new();
        let p = json!({"session_id":"s","hook_event_name":"PostToolUse","tool_name":"Task",
            "transcript_path":"/p/s.jsonl","agent_id":"L",
            "tool_response":{"status":"async_launched","agentId":"A"}});
        apply_event(&mut reg, &p, 1).unwrap();
        let r = &reg["s"].agents["A"];
        assert_eq!(r.launcher.as_deref(), Some("L"));
        assert_eq!(r.transcript_path, "/p/s/subagents/agent-A.jsonl");
    }

    #[test]
    fn partial_trailing_line_is_skipped_but_inner_garbage_is_not() {
        let good = json!({"type":"user","timestamp":"2026-01-01T00:00:00Z",
            "message":{"content":"go"}})
        .to_string();
        assert!(parse_transcript(&format!("{good}\n{{\"type\":")).is_ok());
        assert!(parse_transcript(&format!("{{oops\n{good}\n")).is_err());
    }

    #[test]
    fn handback_error_result_is_not_success() {
        let b = json!({"type":"tool_result","is_error":true,
            "content":"{\"success\":true}"});
        assert!(!handback_success(&b));
        let b = json!({"type":"tool_result","content":[{"type":"text",
            "text":"{\"success\":true,\"message\":\"Report delivered to your caller.\"}"}]});
        assert!(handback_success(&b));
        let b = json!({"type":"tool_result","content":"{\"success\":false}"});
        assert!(!handback_success(&b));
    }

    #[test]
    fn resumed_after_stop_is_not_ended() {
        let rec = AgentRecord {
            agent_id: "a".into(),
            stops: 1,
            last_stop_at: Some(100),
            ..Default::default()
        };
        let t = format!(
            "{}\n",
            json!({"type":"user","timestamp":"1970-01-01T00:03:20Z",
                "message":{"content":[{"type":"tool_result","tool_use_id":"x","content":"ok"}]}})
        );
        let c = classify(&rec, &Determination::Known(Some(t)), &Ok(vec![]), 210, 600);
        assert_eq!(c.state, State::Progressing, "{c:?}");
    }
}
