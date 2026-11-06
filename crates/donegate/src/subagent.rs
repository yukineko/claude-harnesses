//! `donegate subagent-gate` — the SubagentStop hook (backlog 95deaa32).
//!
//! Judges the roots the STOPPING subagent wrote (its own transcript,
//! `agent_transcript_path`, mapped to git roots exactly as the parent's Stop
//! maps writes), with the same per-root logic as the parent's Stop
//! ([`crate::rootcheck::judge`]): checks run, a red root blocks the subagent's
//! stop (`{"decision":"block"}`), an identical repeat is waived loudly, and
//! `max_attempts` bounds the chain. The attempt counter and the waiver memory
//! are keyed `<session>-agent-<agent_id>`, so a subagent never shares the
//! parent's chain (or another agent's).
//!
//! This is what lets the parent's Stop leave a root that only a still-running
//! background subagent wrote to that subagent ([`crate::inflight`]). The
//! parent's Stop judges the root again once the subagent has finished.
//!
//! Fail-closed: a payload naming no locatable agent transcript, an unreadable
//! transcript, or roots that cannot be determined BLOCK the subagent's stop
//! (bounded by the shared repeat ledger: a second identical block is waived
//! with a visible message). A subagent that wrote nothing has nothing to judge
//! and is allowed silently. Roots with no donegate config, or a config that
//! declares no checks, are not judged here (the parent's Stop judges them
//! after the subagent finishes); a root whose declared checks donegate refuses
//! to run blocks, as on the parent's Stop.
//!
//! The one-shot `donegate skip` is NOT consumed here: it is keyed by session,
//! which a subagent shares with its parent, so consuming it here would spend
//! the parent's skip.

use std::path::{Path, PathBuf};

use harness_core::verdict::Determination;
use serde_json::Value;

use crate::config::Config;
use crate::model::HookInput;
use crate::{rootcheck, state};

/// Panic-barrier / payload-reader name: distinct from the parent's `donegate`
/// so a subagent's panic streak never counts toward the parent's.
const GATE: &str = "donegate-subagent";

pub(crate) fn subagent_gate_command() -> ! {
    use std::io::IsTerminal;
    let raw = harness_core::gate::run::read_stop_payload_guarded(
        GATE,
        std::io::stdin().is_terminal(),
        harness_core::hook::try_read_stdin(),
    );
    let hook = HookInput::parse(&raw);
    let value = serde_json::from_str::<Value>(raw.trim()).ok();
    let interactive = hook.is_none();
    let stop_hook_active = hook.as_ref().is_some_and(|h| h.stop_hook_active);
    harness_core::gate::run::run_guarded(GATE, interactive, stop_hook_active, move || {
        run(hook, value)
    })
}

fn str_field<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("").trim()
}

/// The stopping subagent's transcript: `agent_transcript_path`, else
/// `<parent transcript dir>/<parent stem>/subagents/agent-<agent_id>.jsonl`.
pub(crate) fn agent_transcript(v: &Value) -> Option<PathBuf> {
    let explicit = str_field(v, "agent_transcript_path");
    if !explicit.is_empty() {
        return Some(PathBuf::from(explicit));
    }
    let id = str_field(v, "agent_id");
    let tp = Path::new(str_field(v, "transcript_path"));
    if id.is_empty() || tp.as_os_str().is_empty() {
        return None;
    }
    let stem = tp.file_stem()?;
    Some(
        tp.parent()?
            .join(stem)
            .join("subagents")
            .join(format!("agent-{id}.jsonl")),
    )
}

fn block(header: &str, why: &str) -> ! {
    let reason = format!("{header}\ndonegate: BLOCKED - {why}");
    harness_core::repeat::emit_stop_block_keyed("donegate", &reason, &reason);
    std::process::exit(0);
}

/// The roots this subagent owns: every written toplevel, plus the hook cwd
/// when a write lies in it (or a write's directory is gone, so it cannot be
/// shown that it does not) and no other owned root already contains it.
pub(crate) fn owned_roots(
    roots: &[PathBuf],
    writes: &harness_core::attribution::SessionWrites,
) -> Vec<PathBuf> {
    let Some((cwd, rest)) = roots.split_first() else {
        return Vec::new();
    };
    let mut out = rest.to_vec();
    let touches_cwd = writes
        .edited
        .iter()
        .chain(writes.bash.iter())
        .any(|p| p.starts_with(cwd) || p.parent().is_none_or(|d| !d.is_dir()));
    if touches_cwd && !rest.iter().any(|r| cwd.starts_with(r)) {
        out.insert(0, cwd.clone());
    }
    out
}

fn run(hook: Option<HookInput>, value: Option<Value>) -> ! {
    let (Some(input), Some(v)) = (hook, value) else {
        eprintln!(
            "donegate subagent-gate: no SubagentStop payload on stdin; nothing can be judged"
        );
        std::process::exit(1);
    };
    if Config::disabled_env() {
        std::process::exit(0);
    }
    let agent_t = agent_transcript(&v);
    let id = match str_field(&v, "agent_id") {
        "" => agent_t
            .as_deref()
            .and_then(crate::inflight::agent_id_of)
            .unwrap_or_default(),
        s => s.to_string(),
    };
    let header = format!(
        "donegate (SubagentStop of agent {}):",
        if id.is_empty() { "<unknown>" } else { &id }
    );
    let Some(agent_t) = agent_t else {
        block(
            &header,
            "the SubagentStop payload names no agent_transcript_path and no agent_id + \
             transcript_path to locate it, so what this subagent wrote cannot be determined.",
        )
    };
    let agent_s = agent_t.to_string_lossy().into_owned();
    let writes = match harness_core::attribution::own_written_paths(&agent_s) {
        Determination::Known(w) => w,
        Determination::Undetermined(why) => block(
            &header,
            &format!("cannot read what this subagent wrote: {}", why.as_str()),
        ),
    };
    if writes.edited.is_empty() && writes.bash.is_empty() {
        std::process::exit(0);
    }
    let session_t = if input.transcript_path.trim().is_empty() {
        agent_s.clone()
    } else {
        input.transcript_path.clone()
    };
    let cwd = input.cwd_or_current();
    let store = harness_core::worktree_origin::default_store();
    let sr = match harness_core::session_roots::roots_of_writes(&cwd, &writes, &session_t, &store) {
        Determination::Known(sr) => sr,
        Determination::Undetermined(why) => block(
            &header,
            &format!(
                "the roots this subagent wrote into cannot be determined ({}); no checks ran.",
                why.as_str()
            ),
        ),
    };
    let session = format!(
        "{}-agent-{}",
        input.session_key(),
        if id.is_empty() { "unknown" } else { &id }
    );
    let waiver_allowed = !input.session_id.trim().is_empty() && !id.is_empty();
    let mut notices: Vec<String> = Vec::new();
    let mut blocks: Vec<String> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    let mut waived_any = false;
    let mut judged_dirs: Vec<PathBuf> = Vec::new();
    for r in owned_roots(&sr.roots, &writes) {
        let (rcfg, rdecl) = Config::resolve(&r);
        if rdecl.is_refusal() {
            let b = format!(
                "[root: {}] donegate refuses to run this root's checks: {}",
                r.display(),
                crate::declaration_note(&r, &rdecl)
            );
            keys.push(b.clone());
            blocks.push(b);
            continue;
        }
        if !rcfg.enabled || rcfg.checks.is_empty() {
            continue;
        }
        if !judged_dirs.contains(&rcfg.state_dir) {
            judged_dirs.push(rcfg.state_dir.clone());
        }
        match rootcheck::judge(&rcfg, &r, &session, waiver_allowed, &mut notices) {
            rootcheck::Judged::Green | rootcheck::Judged::GaveUp => {}
            rootcheck::Judged::Waived => waived_any = true,
            rootcheck::Judged::Blocked { reason, key } => {
                blocks.push(reason);
                keys.push(key);
            }
        }
    }
    if !blocks.is_empty() {
        let mut reason = format!("{header}\n{}", blocks.join("\n\n"));
        // The shared reason text names `donegate skip`, which this hook does
        // not consume (see the module docs); say so rather than mislead.
        reason.push_str(
            "\n\n(At SubagentStop `donegate skip` is NOT consumed: it belongs to the parent \
             session. This block is bounded by max_attempts and the identical-finding waiver.)",
        );
        crate::append_notices(&mut reason, &notices);
        let key = format!("{header}\n{}", keys.join("\n\n"));
        harness_core::repeat::emit_stop_block_keyed("donegate", &reason, &key);
        std::process::exit(0);
    }
    if !waived_any {
        // Nothing red remains standing: the agent's chain has ended.
        for d in &judged_dirs {
            state::reset(d, &session);
        }
    }
    if !notices.is_empty() {
        notices.insert(0, header);
    }
    crate::emit_notices(&notices, false);
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn agent_transcript_prefers_the_explicit_path_then_derives_it() {
        let v = serde_json::json!({"agent_transcript_path": "/x/a.jsonl", "agent_id": "q"});
        assert_eq!(agent_transcript(&v), Some(PathBuf::from("/x/a.jsonl")));
        let v = serde_json::json!({"transcript_path": "/p/S.jsonl", "agent_id": "q"});
        assert_eq!(
            agent_transcript(&v),
            Some(PathBuf::from("/p/S/subagents/agent-q.jsonl"))
        );
        let v = serde_json::json!({"transcript_path": "/p/S.jsonl"});
        assert_eq!(agent_transcript(&v), None);
    }

    fn writes(edited: &[&Path]) -> harness_core::attribution::SessionWrites {
        harness_core::attribution::SessionWrites {
            edited: edited.iter().map(|p| p.to_path_buf()).collect(),
            bash: BTreeSet::new(),
        }
    }

    #[test]
    fn owned_roots_include_the_cwd_only_when_written() {
        let d = std::env::temp_dir().join(format!("dg-owned-{}", std::process::id()));
        let cwd = d.join("cwd");
        let wt = d.join("wt");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        let roots = vec![cwd.clone(), wt.clone()];
        assert_eq!(
            owned_roots(&roots, &writes(&[&wt.join("f.rs")])),
            vec![wt.clone()]
        );
        assert_eq!(
            owned_roots(&roots, &writes(&[&cwd.join("f.rs")])),
            vec![cwd.clone(), wt.clone()]
        );
        // A write whose directory is gone cannot be shown to lie outside cwd.
        assert_eq!(
            owned_roots(&roots, &writes(&[&d.join("gone/f.rs")])),
            vec![cwd.clone(), wt.clone()]
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
