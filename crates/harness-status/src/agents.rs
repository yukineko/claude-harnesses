//! Classify what a background subagent is doing from facts observable right now.
//!
//! Principle (CLAUDE.md §1/§3): an undeterminable state is `Unknown` with its
//! candidates listed, never "fine", never silence. Every confident variant is
//! backed by an observed fact:
//!
//! * `Finished` - the transcript records a `SubagentStop` hook_success (not a
//!   blocking one) after the last assistant message, which carries no tool_use.
//! * `RateLimited` - the last conversation entry is an `isApiErrorMessage`
//!   assistant entry with `error: "rate_limit"`.
//! * `AskPending` - a tool_use with no tool_result whose PreToolUse hook answered
//!   `permissionDecision: ask` for that very `toolUseID`.
//! * `ToolRunning` - a pending Bash tool_use whose command text appears in a
//!   `sh -c` process of the given process listing.
//! * `Asleep` - the last `pmset -g log` Sleep/Wake event at or before `now` is a
//!   Sleep at or after the last transcript entry.
//!
//! Anything else - unreadable transcript, unreadable `ps` / `pmset`, a pending
//! tool with no matching process, an idle transcript - is `Unknown`.

use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// What a subagent is doing right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentState {
    /// Last entries: assistant `end_turn` text, then a `SubagentStop` hook_success.
    Finished,
    /// Last assistant tool_use has no tool_result and a PreToolUse hook answered
    /// `permissionDecision: ask`. Only a transcript-side fact: print as
    /// "ask-pending (if not asleep)".
    AskPending { reason: String },
    /// Last entry is an `isApiErrorMessage` with error `rate_limit`.
    RateLimited { text: String },
    /// A pending tool_use whose command text appears in a `zsh -c` child process.
    ToolRunning { command: String },
    /// The machine was asleep at `now`; `underlying` is the state the
    /// transcript alone implies. (Sleep for part of the gap while awake at `now`
    /// is not `Asleep`; it is reported in `Unknown::why` instead.)
    Asleep {
        since: String,
        underlying: Box<AgentState>,
    },
    /// Not determinable. `candidates` lists what it could be; `why` says what could
    /// not be read or distinguished.
    Unknown {
        candidates: Vec<String>,
        why: String,
    },
}

impl AgentState {
    /// One-line human rendering used by the CLI.
    pub fn render(&self) -> String {
        match self {
            AgentState::Finished => "finished".to_string(),
            AgentState::AskPending { reason } => {
                format!("ask-pending (if not asleep): {}", one_line(reason, 160))
            }
            AgentState::RateLimited { text } => format!("rate-limited: {}", one_line(text, 160)),
            AgentState::ToolRunning { command } => {
                format!("tool-running: {}", one_line(command, 100))
            }
            AgentState::Asleep { since, underlying } => {
                format!("asleep since {since} (underlying: {})", underlying.render())
            }
            AgentState::Unknown { candidates, why } => format!(
                "unknown (candidates: {}; why: {})",
                candidates.join(" | "),
                one_line(why, 300)
            ),
        }
    }
}

fn one_line(s: &str, max: usize) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = flat.trim();
    if flat.chars().count() > max {
        let cut: String = flat.chars().take(max).collect();
        format!("{cut}...")
    } else {
        flat.to_string()
    }
}

// ---- transcript facts ------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Conv {
    /// Assistant entry without a tool_use block. `api_error` = the `error`
    /// field when `isApiErrorMessage` is true.
    AssistantText {
        api_error: Option<String>,
        text: String,
    },
    AssistantToolUse,
    User,
}

#[derive(Debug, Clone)]
struct Pending {
    id: String,
    name: String,
    command: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct Facts {
    /// Latest entry timestamp (epoch seconds) and its raw text.
    last_ts: Option<(i64, String)>,
    /// tool_use entries with no tool_result yet, in transcript order.
    pending: Vec<Pending>,
    /// PreToolUse `ask` answers keyed by toolUseID.
    asks: HashMap<String, String>,
    last_conv: Option<Conv>,
    /// A non-blocking SubagentStop hook_success was recorded after `last_conv`.
    stop_after_last_conv: bool,
    /// A SubagentStop hook asked to block the stop after `last_conv`.
    stop_blocked_after_last_conv: bool,
}

fn parse_facts(transcript: &str) -> Result<Facts, String> {
    if transcript.trim().is_empty() {
        return Err("cannot read transcript: it is empty".to_string());
    }
    let mut f = Facts::default();
    for (i, raw) in transcript.lines().enumerate() {
        if raw.trim().is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(raw) {
            Ok(v @ Value::Object(_)) => v,
            Ok(_) | Err(_) => {
                return Err(format!(
                    "cannot read transcript: line {} is not a JSON object",
                    i + 1
                ))
            }
        };
        if let Some(ts) = v.get("timestamp").and_then(Value::as_str) {
            if let Some(e) = parse_rfc3339(ts) {
                if f.last_ts.as_ref().is_none_or(|(prev, _)| e >= *prev) {
                    f.last_ts = Some((e, ts.to_string()));
                }
            }
        }
        match v.get("type").and_then(Value::as_str) {
            Some("assistant") => {
                let content = v.pointer("/message/content");
                let mut has_tool_use = false;
                let mut text = String::new();
                if let Some(Value::Array(items)) = content {
                    for it in items {
                        match it.get("type").and_then(Value::as_str) {
                            Some("tool_use") => {
                                has_tool_use = true;
                                let id = it.get("id").and_then(Value::as_str).unwrap_or("");
                                f.pending.push(Pending {
                                    id: id.to_string(),
                                    name: it
                                        .get("name")
                                        .and_then(Value::as_str)
                                        .unwrap_or("?")
                                        .to_string(),
                                    command: it
                                        .pointer("/input/command")
                                        .and_then(Value::as_str)
                                        .map(str::to_string),
                                });
                            }
                            Some("text") => {
                                if let Some(t) = it.get("text").and_then(Value::as_str) {
                                    text.push_str(t);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                let api_error = if v.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true)
                {
                    Some(
                        v.get("error")
                            .and_then(Value::as_str)
                            .unwrap_or("unspecified")
                            .to_string(),
                    )
                } else {
                    None
                };
                f.last_conv = Some(if has_tool_use {
                    Conv::AssistantToolUse
                } else {
                    Conv::AssistantText { api_error, text }
                });
                f.stop_after_last_conv = false;
                f.stop_blocked_after_last_conv = false;
            }
            Some("user") => {
                if let Some(Value::Array(items)) = v.pointer("/message/content") {
                    for it in items {
                        if it.get("type").and_then(Value::as_str) == Some("tool_result") {
                            if let Some(id) = it.get("tool_use_id").and_then(Value::as_str) {
                                f.pending.retain(|p| p.id != id);
                            }
                        }
                    }
                }
                f.last_conv = Some(Conv::User);
                f.stop_after_last_conv = false;
                f.stop_blocked_after_last_conv = false;
            }
            Some("attachment") => {
                let Some(a) = v.get("attachment") else {
                    continue;
                };
                let event = a
                    .get("hookEvent")
                    .and_then(Value::as_str)
                    .or_else(|| a.get("hookName").and_then(Value::as_str))
                    .unwrap_or("");
                let atype = a.get("type").and_then(Value::as_str).unwrap_or("");
                let stdout = a.get("stdout").and_then(Value::as_str).unwrap_or("");
                let out: Option<Value> = serde_json::from_str(stdout.trim()).ok();
                if event == "SubagentStop" {
                    let blocks = out
                        .as_ref()
                        .and_then(|o| o.get("decision"))
                        .and_then(Value::as_str)
                        == Some("block");
                    if atype == "hook_success" && !blocks {
                        f.stop_after_last_conv = true;
                    } else if blocks || atype == "hook_blocking_error" {
                        // `hook_non_blocking_error` does not stop the stop: neither.
                        f.stop_blocked_after_last_conv = true;
                    }
                } else if event == "PreToolUse" {
                    let hso = out.as_ref().and_then(|o| o.get("hookSpecificOutput"));
                    let decision = hso
                        .and_then(|h| h.get("permissionDecision"))
                        .and_then(Value::as_str);
                    if decision == Some("ask") {
                        if let Some(id) = a.get("toolUseID").and_then(Value::as_str) {
                            let reason = hso
                                .and_then(|h| h.get("permissionDecisionReason"))
                                .and_then(Value::as_str)
                                .unwrap_or("(no reason given)");
                            f.asks.insert(id.to_string(), reason.to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(f)
}

fn is_finished(f: &Facts) -> bool {
    matches!(
        f.last_conv,
        Some(Conv::AssistantText {
            api_error: None,
            ..
        })
    ) && f.pending.is_empty()
        && f.stop_after_last_conv
        && !f.stop_blocked_after_last_conv
}

fn s(v: &str) -> String {
    v.to_string()
}

/// The state the transcript (plus the process listing) implies, before the
/// sleep overlay.
fn base_state(f: &Facts, ps: Option<&str>) -> AgentState {
    if is_finished(f) {
        return AgentState::Finished;
    }
    let Some(last) = &f.last_conv else {
        return AgentState::Unknown {
            candidates: vec![s("not started yet"), s("agent died before its first entry")],
            why: s("cannot read a state: no user/assistant entries in the transcript"),
        };
    };
    if let Conv::AssistantText {
        api_error: Some(err),
        text,
    } = last
    {
        if err == "rate_limit" {
            return AgentState::RateLimited { text: text.clone() };
        }
        return AgentState::Unknown {
            candidates: vec![
                format!("waiting to retry after API error `{err}`"),
                s("agent died after the API error"),
            ],
            why: format!(
                "last entry is an API error `{err}`: {}",
                one_line(text, 120)
            ),
        };
    }
    if !f.pending.is_empty() {
        if let Some(reason) = f.pending.iter().rev().find_map(|p| f.asks.get(&p.id)) {
            return AgentState::AskPending {
                reason: reason.clone(),
            };
        }
        let p = f.pending.last().expect("non-empty");
        let Some(cmd) = &p.command else {
            let mut candidates = vec![format!("tool `{}` still in progress", p.name)];
            if p.name == "Agent" || p.name == "Task" {
                candidates.push(s("waiting on a nested subagent"));
            }
            candidates.push(s("agent died"));
            return AgentState::Unknown {
                candidates,
                why: format!(
                    "pending `{}` tool_use {} has no shell command to look for in ps",
                    p.name, p.id
                ),
            };
        };
        let Some(ps) = ps else {
            return AgentState::Unknown {
                candidates: vec![
                    s("tool still running"),
                    s("tool process died"),
                    s("tool not yet spawned"),
                ],
                why: format!(
                    "ps unreadable: cannot tell whether the pending command `{}` is alive",
                    one_line(cmd, 80)
                ),
            };
        };
        if ps_has_command(ps, cmd) {
            return AgentState::ToolRunning {
                command: cmd.clone(),
            };
        }
        return AgentState::Unknown {
            candidates: vec![
                s("tool process died, or already exited with its result not yet recorded"),
                s("tool not yet spawned"),
            ],
            why: format!(
                "no `sh -c` process carries the pending command `{}`",
                one_line(cmd, 80)
            ),
        };
    }
    let mut candidates = vec![
        s("slow model generation"),
        s("machine sleep interrupted the API stream"),
        s("agent died"),
    ];
    let why = match last {
        Conv::AssistantText { .. } if f.stop_blocked_after_last_conv => {
            candidates.insert(
                0,
                s("continuing after a SubagentStop hook blocked the stop"),
            );
            s("a SubagentStop hook blocked the stop; no later entry")
        }
        Conv::AssistantText { .. } => {
            candidates.push(s("SubagentStop hook still running or not recorded"));
            s("last entry is assistant text with no SubagentStop recorded after it")
        }
        Conv::User => s("last entry is a user/tool_result entry; no assistant reply yet"),
        Conv::AssistantToolUse => s("last entry is a tool_use that already has its result"),
    };
    AgentState::Unknown { candidates, why }
}

fn ps_has_command(ps: &str, command: &str) -> bool {
    let Some(first) = command.lines().map(str::trim).find(|l| !l.is_empty()) else {
        return false;
    };
    // Claude Code runs Bash commands as `zsh -c ... eval '<command>'`, so single
    // quotes in the command appear as `'\''` in the process listing.
    let escaped = first.replace('\'', "'\\''");
    ps.lines()
        .any(|l| l.contains("sh -c ") && (l.contains(first) || l.contains(&escaped)))
}

fn append_why(state: &mut AgentState, extra: &str) {
    if let AgentState::Unknown { why, .. } = state {
        if !why.contains(extra) {
            why.push_str("; ");
            why.push_str(extra);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Power {
    Sleep,
    Wake,
}

/// Sleep/Wake events from `pmset -g log` text: (epoch, kind, raw date text).
fn pmset_events(log: &str) -> Vec<(i64, Power, String)> {
    let mut out = Vec::new();
    for l in log.lines() {
        let tok: Vec<&str> = l.split_whitespace().collect();
        if tok.len() < 4 {
            continue;
        }
        let kind = match (tok[3], tok.get(4).copied()) {
            ("Sleep", _) => Power::Sleep,
            ("Wake", Some("Requests")) => continue,
            ("Wake", _) | ("DarkWake", _) | ("Start", _) => Power::Wake,
            _ => continue,
        };
        let date = format!("{} {} {}", tok[0], tok[1], tok[2]);
        if let Some(e) = parse_pmset_time(&date) {
            out.push((e, kind, date));
        }
    }
    out.sort_by_key(|e| e.0);
    out
}

fn overlay(
    base: AgentState,
    f: &Facts,
    ps: Option<&str>,
    pmset: Option<&str>,
    now: &str,
) -> AgentState {
    if base == AgentState::Finished {
        return base;
    }
    let mut st = base;
    if ps.is_none() {
        append_why(&mut st, "ps unreadable");
    }
    let Some(now_e) = parse_rfc3339(now) else {
        append_why(
            &mut st,
            "current time unparseable: cannot tell whether the machine is asleep",
        );
        return st;
    };
    let Some(log) = pmset else {
        append_why(
            &mut st,
            "pmset unreadable: cannot tell whether the machine slept",
        );
        return st;
    };
    let events = pmset_events(log);
    let upto: Vec<&(i64, Power, String)> = events.iter().filter(|e| e.0 <= now_e).collect();
    let Some(last_ev) = upto.last() else {
        append_why(
            &mut st,
            "pmset log has no Sleep/Wake line before now: cannot tell whether the machine slept",
        );
        return st;
    };
    let last_ts = f.last_ts.as_ref().map(|(e, _)| *e);
    if last_ev.1 == Power::Sleep {
        if last_ts.is_none_or(|t| last_ev.0 >= t) {
            return AgentState::Asleep {
                since: last_ev.2.clone(),
                underlying: Box::new(st),
            };
        }
        append_why(
            &mut st,
            "pmset's last event is a Sleep older than the last transcript entry (log incomplete?)",
        );
        return st;
    }
    match last_ts {
        None => append_why(
            &mut st,
            "no transcript timestamp: cannot measure sleep since the last entry",
        ),
        Some(t) if t < now_e => {
            let slept = slept_secs(&upto, t, now_e);
            // pmset's log rotates: when it starts after the last entry, the sleep
            // before its first line is unobserved, so the figure is only a floor.
            let first = upto[0];
            if first.0 > t {
                append_why(
                    &mut st,
                    &format!(
                        "machine slept at least {}m of the {}m since the last entry (pmset log only starts {}; earlier sleep unobserved)",
                        slept / 60,
                        (now_e - t) / 60,
                        first.2
                    ),
                );
            } else if slept > 0 {
                append_why(
                    &mut st,
                    &format!(
                        "machine slept {}m of the {}m since the last entry (pmset)",
                        slept / 60,
                        (now_e - t) / 60
                    ),
                );
            }
        }
        Some(_) => {}
    }
    st
}

/// Seconds the machine was asleep within [from, to], from sorted events.
fn slept_secs(events: &[&(i64, Power, String)], from: i64, to: i64) -> i64 {
    let mut asleep_since: Option<i64> = None;
    // State at `from`: the last event before it.
    if let Some(e) = events.iter().rev().find(|e| e.0 < from) {
        if e.1 == Power::Sleep {
            asleep_since = Some(from);
        }
    }
    let mut total = 0;
    for e in events.iter().filter(|e| e.0 >= from && e.0 <= to) {
        match (e.1, asleep_since) {
            (Power::Sleep, None) => asleep_since = Some(e.0),
            (Power::Wake, Some(s0)) => {
                total += e.0 - s0;
                asleep_since = None;
            }
            _ => {}
        }
    }
    if let Some(s0) = asleep_since {
        total += to - s0;
    }
    total
}

/// Classify one subagent transcript.
///
/// * `transcript_jsonl` - the raw `agent-<id>.jsonl` text.
/// * `ps_output` - process listing (command lines); `None` = could not be read.
/// * `pmset_log` - `pmset -g log` text (Sleep/Wake lines); `None` = could not be read.
/// * `now_utc` - RFC 3339 UTC, e.g. `2026-10-31T16:00:00Z`.
pub fn classify(
    transcript_jsonl: &str,
    ps_output: Option<&str>,
    pmset_log: Option<&str>,
    now_utc: &str,
) -> AgentState {
    match parse_facts(transcript_jsonl) {
        Err(why) => unreadable(why),
        Ok(f) => {
            let base = base_state(&f, ps_output);
            overlay(base, &f, ps_output, pmset_log, now_utc)
        }
    }
}

fn unreadable(why: String) -> AgentState {
    AgentState::Unknown {
        candidates: vec![
            s("transcript mid-write (re-run to tell)"),
            s("corrupt transcript"),
        ],
        why,
    }
}

// ---- time ------------------------------------------------------------------

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn num(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// `YYYY-MM-DD` + `HH:MM:SS` -> epoch seconds (as if UTC).
fn date_time(date: &str, time: &str) -> Option<i64> {
    let d: Vec<&str> = date.split('-').collect();
    let t: Vec<&str> = time.split(':').collect();
    if d.len() != 3 || t.len() != 3 {
        return None;
    }
    let (y, mo, da) = (num(d[0])?, num(d[1])?, num(d[2])?);
    let (h, mi, se) = (num(t[0])?, num(t[1])?, num(t[2])?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&da) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    Some(days_from_civil(y, mo, da) * 86400 + h * 3600 + mi * 60 + se)
}

/// Offset like `+0900` / `+09:00` / `Z` -> seconds east of UTC.
fn offset_secs(off: &str) -> Option<i64> {
    if off == "Z" {
        return Some(0);
    }
    let sign = match off.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits: String = off[1..].chars().filter(|c| *c != ':').collect();
    if digits.len() != 4 {
        return None;
    }
    Some(sign * (num(&digits[..2])? * 3600 + num(&digits[2..])? * 60))
}

fn parse_rfc3339(ts: &str) -> Option<i64> {
    let (date, rest) = ts.split_once('T')?;
    if rest.len() < 8 {
        return None;
    }
    let (time, mut tail) = rest.split_at(8);
    if let Some(frac) = tail.strip_prefix('.') {
        let n = frac.bytes().take_while(u8::is_ascii_digit).count();
        tail = &frac[n..];
    }
    Some(date_time(date, time)? - offset_secs(tail)?)
}

/// `2026-11-01 00:05:00 +0900` -> epoch seconds.
fn parse_pmset_time(s: &str) -> Option<i64> {
    let mut it = s.split_whitespace();
    let (date, time, off) = (it.next()?, it.next()?, it.next()?);
    Some(date_time(date, time)? - offset_secs(off)?)
}

fn now_rfc3339() -> Option<String> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    let (y, m, d) = civil_from_days(secs.div_euclid(86400));
    let r = secs.rem_euclid(86400);
    Some(format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        r / 3600,
        (r % 3600) / 60,
        r % 60
    ))
}

// ---- CLI -------------------------------------------------------------------

/// Run a command; `None` unless it ran and exited 0 with UTF-8-lossy stdout.
fn gather(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

struct Found {
    session: String,
    id: String,
    jsonl: PathBuf,
}

/// Walk `<projects>/<proj>/<session>/subagents/agent-<id>.jsonl`. Directory read
/// failures other than "subagents dir absent" are returned as errors, never
/// skipped silently (a skipped dir would read as "no subagents there").
fn discover(projects_dir: &Path, session: Option<&str>) -> (Vec<Found>, Vec<String>) {
    let mut found = Vec::new();
    let mut errors = Vec::new();
    let projs = match std::fs::read_dir(projects_dir) {
        Ok(r) => r,
        Err(e) => {
            errors.push(format!("cannot read {}: {e}", projects_dir.display()));
            return (found, errors);
        }
    };
    for proj in projs {
        let proj = match proj {
            Ok(p) => p.path(),
            Err(e) => {
                errors.push(format!("cannot read {}: {e}", projects_dir.display()));
                continue;
            }
        };
        if !proj.is_dir() {
            continue;
        }
        let sessions: Vec<PathBuf> = match session {
            Some(sid) => vec![proj.join(sid)],
            None => match std::fs::read_dir(&proj) {
                Ok(r) => r
                    .filter_map(|e| match e {
                        Ok(e) => Some(e.path()),
                        Err(err) => {
                            errors.push(format!("cannot read {}: {err}", proj.display()));
                            None
                        }
                    })
                    .filter(|p| p.is_dir())
                    .collect(),
                Err(e) => {
                    errors.push(format!("cannot read {}: {e}", proj.display()));
                    continue;
                }
            },
        };
        for sdir in sessions {
            let sub = sdir.join("subagents");
            let entries = match std::fs::read_dir(&sub) {
                Ok(r) => r,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    errors.push(format!("cannot read {}: {e}", sub.display()));
                    continue;
                }
            };
            let sname = sdir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            for e in entries {
                let p = match e {
                    Ok(e) => e.path(),
                    Err(err) => {
                        errors.push(format!("cannot read {}: {err}", sub.display()));
                        continue;
                    }
                };
                let Some(name) = p.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                    continue;
                };
                if let Some(id) = name
                    .strip_prefix("agent-")
                    .and_then(|n| n.strip_suffix(".jsonl"))
                {
                    found.push(Found {
                        session: sname.clone(),
                        id: id.to_string(),
                        jsonl: p,
                    });
                }
            }
        }
    }
    found.sort_by(|a, b| a.jsonl.cmp(&b.jsonl));
    (found, errors)
}

fn description(jsonl: &Path, id: &str) -> String {
    let meta = jsonl.with_file_name(format!("agent-{id}.meta.json"));
    match std::fs::read_to_string(&meta) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "(no .meta.json)".to_string(),
        Err(e) => format!("(description unreadable: {e})"),
        Ok(t) => match serde_json::from_str::<Value>(&t) {
            Ok(v) => match v.get("description").and_then(Value::as_str) {
                Some(d) => one_line(d, 80),
                None => "(no description in .meta.json)".to_string(),
            },
            Err(e) => format!("(description unreadable: {e})"),
        },
    }
}

/// CLI entry for `harness-status agents [--session <id>]`: one line per subagent
/// (id, description from `.meta.json`, state, last-entry time). Returns the exit code:
/// 0 when every directory could be read (states may still be `unknown`), 1 when a
/// directory could not be read.
pub fn run_cli(projects_dir: &Path, session: Option<&str>) -> i32 {
    let (found, errors) = discover(projects_dir, session);
    for e in &errors {
        println!("agents: unknown - {e}");
    }
    if found.is_empty() {
        if errors.is_empty() {
            match session {
                Some(sid) => println!("no subagents for session {sid}"),
                None => println!("no subagents under {}", projects_dir.display()),
            }
            return 0;
        }
        return 1;
    }
    let ps = gather("ps", &["-ww", "-axo", "pid,ppid,command"]);
    let now = now_rfc3339();
    // pmset -g log takes seconds; read it once, and only when some transcript's
    // state depends on it (a finished or unreadable transcript does not).
    let mut pmset: Option<Option<String>> = None;
    for a in &found {
        let desc = description(&a.jsonl, &a.id);
        let (state, last) = match std::fs::read_to_string(&a.jsonl) {
            Err(e) => (
                AgentState::Unknown {
                    candidates: vec![s("transcript removed or unreadable")],
                    why: format!("cannot read {}: {e}", a.jsonl.display()),
                },
                None,
            ),
            Ok(text) => match parse_facts(&text) {
                Err(why) => (unreadable(why), None),
                Ok(f) => {
                    let base = base_state(&f, ps.as_deref());
                    let st = if base == AgentState::Finished {
                        base
                    } else {
                        let pm = pmset.get_or_insert_with(|| gather("pmset", &["-g", "log"]));
                        match &now {
                            Some(now) => overlay(base, &f, ps.as_deref(), pm.as_deref(), now),
                            None => overlay(base, &f, ps.as_deref(), pm.as_deref(), ""),
                        }
                    };
                    (st, f.last_ts.map(|(_, t)| t))
                }
            },
        };
        let prefix = if session.is_none() {
            format!("[{}] ", a.session.get(..8).unwrap_or(&a.session))
        } else {
            String::new()
        };
        println!(
            "{prefix}{}  {}  {}  last={}",
            a.id,
            desc,
            state.render(),
            last.as_deref().unwrap_or("unknown")
        );
    }
    i32::from(!errors.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_and_pmset_times_agree() {
        assert_eq!(
            parse_rfc3339("2026-10-31T15:05:00.000Z"),
            parse_pmset_time("2026-11-01 00:05:00 +0900")
        );
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("garbage"), None);
    }

    #[test]
    fn civil_round_trip() {
        for z in [-1, 0, 19782, 20627, 20757] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
    }

    #[test]
    fn wake_requests_line_is_not_a_wake() {
        let log = "2026-10-31 22:35:14 +0900 Sleep               \tEntering Sleep\n\
                   2026-10-31 22:35:15 +0900 Wake Requests       \t[x]\n";
        let ev = pmset_events(log);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].1, Power::Sleep);
    }
}
