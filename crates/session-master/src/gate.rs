//! PreToolUse gate (SPEC §5), matcher `Bash|SendMessage`.
//!
//! Deny = a `permissionDecision: deny` JSON on stdout, exit 0. Allow = no
//! output, exit 0 (the normal permission flow decides). Every
//! cannot-determine — unreadable stdin, unparsable payload, unresolvable
//! group, state IO / lock failure, invalid clock, classifier failure — is a
//! Deny when the call could be master-only. The caller wraps this in
//! `harness_core::hook::run_pretooluse_guarded`, so a panic exits 2 (block).

use crate::detect::{self, Op};
use crate::relay::{self, RelayDecision};
use crate::state::{self, State};
use harness_core::verdict::Required;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// What the gate decided; `run` prints it.
#[derive(Debug, PartialEq, Eq)]
pub enum GateOut {
    Allow,
    Deny(String),
}

pub fn deny_json(reason: &str) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    })
}

pub fn run() {
    let out = match harness_core::hook::try_read_stdin() {
        Ok(raw) => decide(&raw),
        Err(e) => GateOut::Deny(format!(
            "session-master gate: hook payload を読めない（判定不能: {e}）ので Deny"
        )),
    };
    if let GateOut::Deny(reason) = out {
        println!("{}", deny_json(&reason));
    }
}

fn str_field<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty())
}

pub fn decide(raw: &str) -> GateOut {
    let payload: Value = match serde_json::from_str(raw.trim()) {
        Ok(v @ Value::Object(_)) => v,
        Ok(_) => {
            return GateOut::Deny(
                "session-master gate: payload が JSON object でない（判定不能）ので Deny".into(),
            )
        }
        Err(e) => {
            return GateOut::Deny(format!(
                "session-master gate: payload を parse できない（判定不能: {e}）ので Deny"
            ))
        }
    };
    let session = str_field(&payload, "session_id").map(str::to_string);
    let cwd = str_field(&payload, "cwd").map(PathBuf::from);
    match str_field(&payload, "tool_name") {
        Some("Bash") => bash(&payload, session.as_deref(), cwd.as_deref()),
        Some("SendMessage") => send(&payload, session.as_deref(), cwd.as_deref()),
        // Not ours (matcher misconfigured): still heartbeat, decide nothing.
        _ => {
            heartbeat_only(session.as_deref(), cwd.as_deref());
            GateOut::Allow
        }
    }
}

fn heartbeat_only(session: Option<&str>, cwd: Option<&Path>) {
    let (Some(s), Some(c)) = (session, cwd) else {
        return;
    };
    let cwd_s = c.display().to_string();
    if let Err(e) = state::with_state(c, |st, ctx| {
        st.touch(s, &cwd_s, ctx.now);
        st.elect(ctx.now, ctx.ttl);
    }) {
        // Not a verdict: the call is not master-only, so its allow does not
        // depend on the heartbeat. Still surfaced, never silent.
        eprintln!("session-master gate: heartbeat できなかった: {e}");
    }
}

/// What the locked transaction for a master-only Bash call observed.
enum Role {
    Master,
    Member { id: String, master: String },
    MemberControl { master: String },
}

fn bash(payload: &Value, session: Option<&str>, cwd: Option<&Path>) -> GateOut {
    let Some(command) = payload
        .get("tool_input")
        .and_then(|t| t.get("command"))
        .and_then(Value::as_str)
    else {
        return GateOut::Deny(
            "session-master gate: Bash の tool_input.command が無い（判定不能）ので Deny".into(),
        );
    };
    let cwd_for_detect = match cwd {
        Some(c) => c.to_path_buf(),
        None => PathBuf::from("/nonexistent-session-master-cwd"),
    };
    let (op_name, kind) = match detect::classify(command, &cwd_for_detect, session).require() {
        Required::Determined(None) => {
            heartbeat_only(session, cwd);
            return GateOut::Allow;
        }
        Required::Determined(Some(op)) => (op_label(op).to_string(), op.kind().to_string()),
        Required::Blocked(why) => (
            format!("master 専用操作か判定不能（{}）", why.as_str()),
            "other".to_string(),
        ),
    };
    // A control call is not queued: the master has nothing to run for it.
    let enqueue = kind != Op::Control.kind();
    let (Some(me), Some(c)) = (session, cwd) else {
        return GateOut::Deny(format!(
            "session-master gate: {op_name} を検出したが、payload に session_id / cwd が無く役割を判定できない（判定不能）ので Deny。queue にも積めていない。"
        ));
    };
    let cwd_s = c.display().to_string();
    let tx = state::with_state(c, |st: &mut State, ctx| {
        st.touch(me, &cwd_s, ctx.now);
        st.elect(ctx.now, ctx.ttl);
        if st.is_master(me) {
            Role::Master
        } else if !enqueue {
            Role::MemberControl {
                master: st.master_label(),
            }
        } else {
            let q = st.enqueue(me, &kind, command, &cwd_s, None, ctx.now);
            Role::Member {
                id: q.id,
                master: st.master_label(),
            }
        }
    });
    match tx {
        Ok(Role::Master) => GateOut::Allow,
        Ok(Role::MemberControl { master }) => GateOut::Deny(format!(
            "session-master: このセッション ({me}) は member。{op_name} は master だけが行える（または別セッションを名乗る）\
             session-master の呼び出しなので Deny した。queue には積んでいない。master: {master}。\
             member が使えるのは自分の session での register / heartbeat / leave / name / role / status / request / results。"
        )),
        Ok(Role::Member { id, master }) => GateOut::Deny(format!(
            "session-master: このセッション ({me}) は member。{op_name} は master 専用操作なので Deny した。\
             master: {master}。依頼を master の queue に積んだ（item id {id}）。master が直列に処理し、\
             結果は `session-master results` で受け取れる（次の prompt でも通知される）。"
        )),
        Err(e) => GateOut::Deny(format!(
            "session-master gate: {op_name} を検出したが、役割を判定できない（判定不能: {e}）ので Deny。\
             queue には積めていない。"
        )),
    }
}

fn op_label(op: Op) -> &'static str {
    match op {
        Op::Push => "push (git push)",
        Op::Rollout => "rollout (scripts/rollout-plugins.sh)",
        Op::Merge => "main への merge",
        Op::Control => "session-master の master 専用 / 他セッション名義の CLI 呼び出し",
    }
}

fn send(payload: &Value, session: Option<&str>, cwd: Option<&Path>) -> GateOut {
    let Some(to) = payload
        .get("tool_input")
        .and_then(|t| t.get("to"))
        .and_then(Value::as_str)
    else {
        return GateOut::Deny(
            "session-master gate: SendMessage の宛先 (to) が無い（判定不能）ので Deny".into(),
        );
    };
    let (Some(me), Some(c)) = (session, cwd) else {
        return GateOut::Deny(
            "session-master gate: payload に session_id / cwd が無く役割を判定できない（判定不能）ので SendMessage を Deny".into(),
        );
    };
    let transcript = str_field(payload, "transcript_path").map(PathBuf::from);
    let cwd_s = c.display().to_string();
    let tx = state::with_state(c, |st, ctx| {
        st.touch(me, &cwd_s, ctx.now);
        st.elect(ctx.now, ctx.ttl);
        st.clone()
    });
    match tx {
        Ok(st) => match relay::decide(&st, me, to, transcript.as_deref()) {
            RelayDecision::Allow => GateOut::Allow,
            RelayDecision::Deny(r) => GateOut::Deny(r),
        },
        Err(e) => GateOut::Deny(format!(
            "session-master gate: 役割を判定できない（判定不能: {e}）ので SendMessage を Deny"
        )),
    }
}
