//! SendMessage judgement (SPEC §5.2): cross-session relays go to the master
//! only; member↔member is denied; a destination that cannot be confirmed as
//! the master or an in-session subagent is denied (restrictive side).

use crate::state::State;
use serde_json::Value;
use std::io::BufRead;
use std::path::Path;

/// What the gate should do with a SendMessage.
#[derive(Debug, PartialEq, Eq)]
pub enum RelayDecision {
    Allow,
    Deny(String),
}

/// Strip a trailing ` [ref]` from a destination name.
pub fn normalize_to(to: &str) -> &str {
    let t = to.trim();
    if t.ends_with(']') {
        if let Some(pos) = t.rfind(" [") {
            return t[..pos].trim_end();
        }
    }
    t
}

/// Sessions of the group the destination names (by name, exact id, or an id
/// prefix of at least 8 characters).
fn matching_sessions<'a>(st: &'a State, to: &str) -> Vec<&'a String> {
    st.sessions
        .iter()
        .filter(|(id, s)| {
            s.name.as_deref() == Some(to)
                || id.as_str() == to
                || (to.len() >= 8 && id.starts_with(to))
        })
        .map(|(id, _)| id)
        .collect()
}

pub fn decide(st: &State, me: &str, to_raw: &str, transcript: Option<&Path>) -> RelayDecision {
    if st.is_master(me) {
        return RelayDecision::Allow;
    }
    let to = normalize_to(to_raw);
    if to == "main" {
        return RelayDecision::Allow;
    }
    let hits = matching_sessions(st, to);
    if !hits.is_empty() {
        if hits.iter().all(|id| st.is_master(id)) {
            return RelayDecision::Allow;
        }
        return RelayDecision::Deny(format!(
            "session-master: このセッション ({me}) は member。member 同士の relay は禁止。宛先は master のみ \
             （宛先 {to:?} は member）。master: {}。master に裁定・作業を頼むときは SendMessage で master 宛てに送るか、\
             `session-master request --kind relay --command <内容>` で master の queue に積む。",
            st.master_label()
        ));
    }
    match transcript {
        Some(p) => match transcript_knows(p, to) {
            Ok(true) => RelayDecision::Allow,
            Ok(false) => RelayDecision::Deny(unknown_reason(
                st,
                me,
                to,
                "transcript に該当 subagent が無い",
            )),
            Err(e) => RelayDecision::Deny(unknown_reason(st, me, to, &e)),
        },
        None => RelayDecision::Deny(unknown_reason(st, me, to, "transcript_path が無い")),
    }
}

fn unknown_reason(st: &State, me: &str, to: &str, why: &str) -> String {
    format!(
        "session-master: 宛先 {to:?} が master でも自セッションの subagent でもあると確かめられない（判定不能: {why}）ので Deny。\
         このセッション ({me}) は member。master: {}。宛先名は `session-master name <NAME>` で登録される。\
         master へ依頼するなら `session-master request --kind relay --command <内容>`。",
        st.master_label()
    )
}

/// Does the session transcript name `to` as one of its subagents?
fn transcript_knows(path: &Path, to: &str) -> Result<bool, String> {
    let f = std::fs::File::open(path)
        .map_err(|e| format!("transcript {} を開けない ({e})", path.display()))?;
    for line in std::io::BufReader::new(f).lines() {
        let line = line.map_err(|e| format!("transcript {} を読めない ({e})", path.display()))?;
        if !line.contains(to) {
            continue;
        }
        // A line that is not JSON cannot name anything structurally.
        if let Ok(v) = serde_json::from_str::<Value>(&line) {
            if value_knows(&v, to) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn value_knows(v: &Value, to: &str) -> bool {
    match v {
        Value::Object(m) => {
            for k in ["agentId", "agent_id"] {
                if m.get(k).and_then(Value::as_str) == Some(to) {
                    return true;
                }
            }
            let is_agent_use = m.get("type").and_then(Value::as_str) == Some("tool_use")
                && matches!(
                    m.get("name").and_then(Value::as_str),
                    Some("Agent" | "Task")
                );
            if is_agent_use
                && m.get("input")
                    .and_then(|i| i.get("name"))
                    .and_then(Value::as_str)
                    == Some(to)
            {
                return true;
            }
            m.values().any(|x| value_knows(x, to))
        }
        Value::Array(a) => a.iter().any(|x| value_knows(x, to)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ref_suffix() {
        assert_eq!(normalize_to("worker [abc123]"), "worker");
        assert_eq!(normalize_to("worker"), "worker");
    }

    #[test]
    fn agent_tool_use_name_is_known() {
        let v: Value = serde_json::json!({"message":{"content":[{"type":"tool_use","name":"Agent","input":{"name":"w1"}}]}});
        assert!(value_knows(&v, "w1"));
        assert!(!value_knows(&v, "w2"));
    }
}
