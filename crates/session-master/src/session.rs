//! Session lifecycle commands (SPEC §4): `register`, `heartbeat`, `leave`,
//! `name`, `role`, `status`.

use crate::queue::Reply;
use crate::state::{self, liveness, Liveness, State};
use serde_json::{json, Value};
use std::path::Path;

pub fn role_str(st: &State, me: &str) -> &'static str {
    if st.is_master(me) {
        "master"
    } else {
        "member"
    }
}

pub fn register(session: &str, cwd: &Path, name: Option<String>) -> Result<Reply, String> {
    let cwd_s = cwd.display().to_string();
    state::with_state(cwd, |st, ctx| {
        st.touch(session, &cwd_s, ctx.now);
        if let Some(n) = name {
            if let Some(s) = st.sessions.get_mut(session) {
                s.name = Some(n);
            }
        }
        st.elect(ctx.now, ctx.ttl);
        Reply::ok(json!({
            "group": ctx.group.key,
            "common_dir": ctx.group.common_dir,
            "session": session,
            "role": role_str(st, session),
        }))
    })
}

pub fn leave(session: &str, cwd: &Path) -> Result<Reply, String> {
    state::with_state(cwd, |st, ctx| {
        st.sessions.remove(session);
        if st.is_master(session) {
            st.master = None;
        }
        st.elect(ctx.now, ctx.ttl);
        Reply::ok(json!({"group": ctx.group.key, "session": session, "left": true}))
    })
}

pub fn name(session: &str, cwd: &Path, name: &str) -> Result<Reply, String> {
    register(session, cwd, Some(name.to_string()))
}

pub fn role(session: &str, cwd: &Path) -> Result<Reply, String> {
    state::with_state(cwd, |st, ctx| {
        st.elect(ctx.now, ctx.ttl);
        let r = role_str(st, session);
        Reply {
            code: if r == "master" { 0 } else { 1 },
            stdout: Some(r.to_string()),
            stderr: None,
        }
    })
}

pub fn master_state(st: &State, now: i64, ttl: i64) -> &'static str {
    match &st.master {
        None => "none",
        Some(m) => match st.sessions.get(m).map(|s| liveness(s, now, ttl)) {
            Some(Liveness::Alive) => "live",
            Some(Liveness::Unknown) => "undetermined",
            Some(Liveness::Dead) | None => "expired",
        },
    }
}

pub fn status_json(st: &State, group: &str, common_dir: &str, now: i64, ttl: i64) -> Value {
    let master = st.master.as_ref().map(|m| match st.sessions.get(m) {
        Some(s) => json!({
            "session": m,
            "name": s.name,
            "last_heartbeat": s.last_heartbeat,
            "alive": liveness(s, now, ttl).json(),
        }),
        None => json!({"session": m, "name": null, "last_heartbeat": null, "alive": false}),
    });
    let mut members: Vec<(&String, &state::Session)> = st.sessions.iter().collect();
    members.sort_by(|a, b| a.1.registered_at.cmp(&b.1.registered_at).then(a.0.cmp(b.0)));
    let members: Vec<Value> = members
        .into_iter()
        .map(|(id, s)| {
            json!({
                "session": id,
                "name": s.name,
                "registered_at": s.registered_at,
                "last_heartbeat": s.last_heartbeat,
                "alive": liveness(s, now, ttl).json(),
                "role": role_str(st, id),
            })
        })
        .collect();
    let queue: Vec<Value> = st
        .queue
        .iter()
        .map(|i| match serde_json::to_value(i) {
            Ok(v) => v,
            Err(e) => json!({"id": i.id, "error": format!("serialize 失敗: {e}")}),
        })
        .collect();
    json!({
        "group": group,
        "common_dir": common_dir,
        "ttl_secs": ttl,
        "now": now,
        "master_state": master_state(st, now, ttl),
        "master": master,
        "members": members,
        "queue": queue,
    })
}

pub fn status(cwd: &Path) -> Result<Reply, String> {
    state::with_state(cwd, |st, ctx| {
        st.elect(ctx.now, ctx.ttl);
        Reply::ok(status_json(
            st,
            &ctx.group.key,
            &ctx.group.common_dir,
            ctx.now,
            ctx.ttl,
        ))
    })
}
