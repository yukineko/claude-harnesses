//! Lifecycle hooks (SPEC §6).
//!
//! * `session-start` / `prompt`: inject the role as `additionalContext`.
//!   A state failure is NOT silence: the context says the role could not be
//!   determined and that master-only operations will be denied.
//! * `stop`: heartbeat; never blocks (provisional ruling 2026-11-22). A master
//!   with open queue items gets a `systemMessage`. Runs under
//!   `harness_core::gate::run::run_guarded` (the caller wires it).
//! * `session-end`: leave; failure → stderr + exit 1.

use crate::queue;
use crate::state::{self, State};
use serde_json::{json, Value};
use std::path::PathBuf;

pub const UNDETERMINED_CONTEXT: &str =
    "session-master: 役割を判定できない（判定不能）。master 専用操作（push / rollout / main への merge / member 間 relay）は Deny される。";

/// Session id and cwd from a hook payload (falling back to env / process cwd).
pub fn ids(payload: &Value) -> Result<(String, PathBuf), String> {
    let session = match payload.get("session_id").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => match std::env::var("CLAUDE_CODE_SESSION_ID") {
            Ok(s) if !s.is_empty() => s,
            _ => return Err("payload にも env にも session_id が無い".to_string()),
        },
    };
    let cwd = match payload.get("cwd").and_then(Value::as_str) {
        Some(c) if !c.is_empty() => PathBuf::from(c),
        _ => std::env::current_dir().map_err(|e| format!("cwd を決められない ({e})"))?,
    };
    Ok((session, cwd))
}

pub fn parse_payload(raw: &str) -> Result<Value, String> {
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(raw.trim()).map_err(|e| format!("hook payload を parse できない ({e})"))
}

fn context(event: &str, text: &str) -> Value {
    json!({"hookSpecificOutput": {"hookEventName": event, "additionalContext": text}})
}

fn head_line(st: &State) -> String {
    match st.head() {
        Some(i) => format!(
            "先頭 item {} [{}] {} (requester {}, status {})",
            i.id, i.kind, i.command, i.requester, i.status
        ),
        None => "queue は空".to_string(),
    }
}

fn master_lines(st: &State) -> String {
    let (p, ip) = st.open_counts();
    format!(
        "このセッションは master。queue: pending {p} 件 / in_progress {ip} 件。{}。\
         処理手順: `session-master next` → 実行 → `session-master complete <ID> --outcome ok|failed --result <TEXT>`。",
        head_line(st)
    )
}

pub fn session_start(raw: &str) -> String {
    let out = parse_payload(raw).and_then(|p| ids(&p)).and_then(|(me, cwd)| {
        let cwd_s = cwd.display().to_string();
        state::with_state(&cwd, |st, ctx| {
            st.touch(&me, &cwd_s, ctx.now);
            st.elect(ctx.now, ctx.ttl);
            if st.is_master(&me) {
                master_lines(st)
            } else {
                format!(
                    "このセッション ({me}) は member。master: {}。push / rollout / main への merge は Deny され master の queue に積まれる。\
                     member 同士の SendMessage は禁止（宛先は master のみ）。結果は `session-master results` で受け取る。",
                    st.master_label()
                )
            }
        })
    });
    let text = match out {
        Ok(t) => format!("session-master: {t}"),
        Err(e) => format!("{UNDETERMINED_CONTEXT}（原因: {e}）"),
    };
    context("SessionStart", &text).to_string()
}

pub fn prompt(raw: &str) -> String {
    let out = parse_payload(raw)
        .and_then(|p| ids(&p))
        .and_then(|(me, cwd)| {
            let cwd_s = cwd.display().to_string();
            state::with_state(&cwd, |st, ctx| {
                let known_before = st.sessions.contains_key(&me);
                let was_master = st.is_master(&me);
                st.touch(&me, &cwd_s, ctx.now);
                st.elect(ctx.now, ctx.ttl);
                let is_master = st.is_master(&me);
                let mut lines = Vec::new();
                if known_before && was_master != is_master {
                    lines.push(if is_master {
                        "役割が変わった: member → master に昇格した。".to_string()
                    } else {
                        format!(
                            "役割が変わった: master → member に降格した（master: {}）。",
                            st.master_label()
                        )
                    });
                }
                if is_master {
                    lines.push(master_lines(st));
                } else {
                    lines.push(format!(
                        "このセッションは member（master: {}）。",
                        st.master_label()
                    ));
                    let results = queue::take_results(st, &me, true);
                    if !results.is_empty() {
                        lines.push(format!(
                            "master から完了結果が {} 件届いた: {}",
                            results.len(),
                            Value::Array(results)
                        ));
                    }
                }
                lines.join(" ")
            })
        });
    let text = match out {
        Ok(t) => format!("session-master: {t}"),
        Err(e) => format!("{UNDETERMINED_CONTEXT}（原因: {e}）"),
    };
    context("UserPromptSubmit", &text).to_string()
}

/// Stop body: returns the stdout line, if any. Never a block.
pub fn stop(payload: &Value) -> Option<String> {
    let out = ids(payload).and_then(|(me, cwd)| {
        let cwd_s = cwd.display().to_string();
        state::with_state(&cwd, |st, ctx| {
            st.touch(&me, &cwd_s, ctx.now);
            st.elect(ctx.now, ctx.ttl);
            if st.is_master(&me) {
                let (p, ip) = st.open_counts();
                if p + ip > 0 {
                    return Some(format!(
                        "session-master: master の queue が残っている（pending {p} / in_progress {ip}）。{}",
                        head_line(st)
                    ));
                }
            }
            None
        })
    });
    match out {
        Ok(Some(msg)) => Some(json!({"systemMessage": msg}).to_string()),
        Ok(None) => None,
        Err(e) => Some(
            json!({"systemMessage": format!("session-master stop: heartbeat できなかった（判定不能: {e}）。役割と queue は未確認。")})
                .to_string(),
        ),
    }
}

pub fn session_end(raw: &str) -> Result<(), String> {
    let p = parse_payload(raw)?;
    let (me, cwd) = ids(&p)?;
    state::with_state(&cwd, |st, ctx| {
        st.sessions.remove(&me);
        if st.is_master(&me) {
            st.master = None;
        }
        st.elect(ctx.now, ctx.ttl);
    })
}
