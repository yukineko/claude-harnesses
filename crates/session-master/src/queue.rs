//! Master queue operations (SPEC §4): `request`, `next`, `complete`,
//! `results`. The queue is serial: at most one item is `in_progress`, and
//! `next` resumes it instead of starting another.

use crate::state::{self, Item};
use serde_json::{json, Value};
use std::path::Path;

pub const KINDS: [&str; 5] = ["push", "rollout", "merge", "relay", "other"];

/// A CLI outcome: exit code and the stdout line.
pub struct Reply {
    pub code: i32,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

impl Reply {
    pub fn ok(v: Value) -> Reply {
        Reply {
            code: 0,
            stdout: Some(v.to_string()),
            stderr: None,
        }
    }
    pub fn refuse(code: i32, msg: String) -> Reply {
        Reply {
            code,
            stdout: None,
            stderr: Some(msg),
        }
    }
}

fn item_json(i: &Item) -> Value {
    match serde_json::to_value(i) {
        Ok(v) => v,
        Err(e) => json!({"id": i.id, "error": format!("serialize 失敗: {e}")}),
    }
}

pub fn request(
    session: &str,
    cwd: &Path,
    kind: &str,
    command: &str,
    note: Option<String>,
) -> Result<Reply, String> {
    if !KINDS.contains(&kind) {
        return Ok(Reply::refuse(
            2,
            format!("--kind は {} のどれか（{kind:?} は不可）", KINDS.join("|")),
        ));
    }
    let cwd_s = cwd.display().to_string();
    state::with_state(cwd, |st, ctx| {
        st.elect(ctx.now, ctx.ttl);
        let q = st.enqueue(session, kind, command, &cwd_s, note, ctx.now);
        Reply::ok(json!({"id": q.id, "deduped": q.deduped}))
    })
}

pub fn next(session: &str, cwd: &Path) -> Result<Reply, String> {
    state::with_state(cwd, |st, ctx| {
        st.elect(ctx.now, ctx.ttl);
        if !st.is_master(session) {
            return Reply::refuse(
                1,
                format!(
                    "session-master next: {session} は member（master は {}）。queue は master だけが処理する",
                    st.master_label()
                ),
            );
        }
        if let Some(i) = st.queue.iter().find(|i| i.status == "in_progress") {
            return Reply::ok(json!({"item": item_json(i), "resumed": true}));
        }
        match st.queue.iter_mut().find(|i| i.status == "pending") {
            Some(i) => {
                i.status = "in_progress".to_string();
                i.started_at = Some(ctx.now);
                Reply::ok(json!({"item": item_json(i), "resumed": false}))
            }
            None => Reply::ok(json!({"item": null})),
        }
    })
}

pub fn complete(
    session: &str,
    cwd: &Path,
    id: &str,
    outcome: &str,
    result: Option<String>,
) -> Result<Reply, String> {
    let status = match outcome {
        "ok" => "done",
        "failed" => "failed",
        _ => {
            return Ok(Reply::refuse(
                2,
                format!("--outcome は ok|failed（{outcome:?} は不可）"),
            ))
        }
    };
    state::with_state(cwd, |st, ctx| {
        st.elect(ctx.now, ctx.ttl);
        if !st.is_master(session) {
            return Reply::refuse(
                1,
                format!(
                    "session-master complete: {session} は member（master は {}）",
                    st.master_label()
                ),
            );
        }
        match st.queue.iter_mut().find(|i| i.id == id) {
            Some(i) if i.status == "in_progress" => {
                i.status = status.to_string();
                i.completed_at = Some(ctx.now);
                i.outcome = Some(outcome.to_string());
                i.result = result;
                Reply::ok(json!({"item": item_json(i)}))
            }
            Some(i) => Reply::refuse(
                1,
                format!("item {id} は in_progress でない（status {}）", i.status),
            ),
            None => Reply::refuse(1, format!("item {id} が queue に無い")),
        }
    })
}

/// Completed items requested by `session` and not yet delivered.
pub fn take_results(st: &mut state::State, session: &str, ack: bool) -> Vec<Value> {
    let mut out = Vec::new();
    for i in st
        .queue
        .iter_mut()
        .filter(|i| i.requester == session && i.is_completed() && !i.delivered)
    {
        out.push(item_json(i));
        if ack {
            i.delivered = true;
        }
    }
    out
}

pub fn results(session: &str, cwd: &Path, ack: bool) -> Result<Reply, String> {
    state::with_state(cwd, |st, ctx| {
        st.elect(ctx.now, ctx.ttl);
        let r = take_results(st, session, ack);
        Reply::ok(json!({"results": r}))
    })
}
