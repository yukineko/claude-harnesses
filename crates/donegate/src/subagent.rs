//! `donegate subagent-gate` — the SubagentStop hook (backlog 95deaa32).
//!
//! Judges ONLY the hook's own root, the SubagentStop payload's `cwd`. User
//! rulings 2026-11-08, verbatim: "donegateのworktreeテストは問題ありすぎるので
//! 即時に停止" and "donegateそのものが他のworktreeを見たり検証したりテストしたり
//! することは禁止とする". donegate does not resolve, list, inspect or check any
//! other worktree or root the subagent wrote into.
//!
//! When the stopping subagent's own transcript (`agent_transcript_path`)
//! records a write inside the cwd ([`crate::rootcheck::writes_in`]), the cwd's
//! checks run ([`crate::rootcheck::judge`]): a red cwd blocks the subagent's
//! stop (`{"decision":"block"}`), an identical repeat is waived loudly, and
//! `max_attempts` bounds the chain. The attempt counter and the waiver memory
//! are keyed `<session>-agent-<agent_id>`, so a subagent never shares the
//! parent's chain (or another agent's).
//!
//! Fail-closed: a payload naming no locatable agent transcript, or an
//! unreadable transcript, BLOCKS the subagent's stop (bounded by the shared
//! repeat ledger: a second identical block is waived with a visible message).
//! A subagent that wrote nothing inside the cwd has nothing to judge and is
//! allowed silently. A cwd with no donegate config, or a config that declares
//! no checks, is not judged here (the parent's Stop judges it); a cwd whose
//! declared checks donegate refuses to run blocks, as on the parent's Stop.
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

/// `agent-<id>.jsonl` -> `<id>`.
pub(crate) fn agent_id_of(sidechain: &Path) -> Option<String> {
    let stem = sidechain.file_stem()?.to_str()?;
    let id = stem.strip_prefix("agent-")?;
    (!id.is_empty()).then(|| id.to_string())
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
        "" => agent_t.as_deref().and_then(agent_id_of).unwrap_or_default(),
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
    let cwd = input.cwd_or_current();
    // Only the hook's own root (the cwd) is judged; a subagent that wrote
    // nothing inside it has nothing donegate may judge (user rulings
    // 2026-11-08, see the module docs).
    if !rootcheck::writes_in(&cwd, &writes) {
        std::process::exit(0);
    }
    let session = format!(
        "{}-agent-{}",
        input.session_key(),
        if id.is_empty() { "unknown" } else { &id }
    );
    let waiver_allowed = !input.session_id.trim().is_empty() && !id.is_empty();
    let mut notices: Vec<String> = Vec::new();
    let (rcfg, rdecl) = Config::resolve(&cwd);
    if rdecl.is_refusal() {
        let b = format!(
            "[root: {}] donegate refuses to run this root's checks: {}",
            cwd.display(),
            crate::declaration_note(&cwd, &rdecl)
        );
        let reason = format!("{header}\n{b}");
        harness_core::repeat::emit_stop_block_keyed("donegate", &reason, &reason);
        std::process::exit(0);
    }
    if !rcfg.enabled || rcfg.checks.is_empty() {
        std::process::exit(0);
    }
    match rootcheck::judge(&rcfg, &cwd, &session, waiver_allowed, &mut notices) {
        rootcheck::Judged::Green | rootcheck::Judged::GaveUp => {
            // Nothing red remains standing: the agent's chain has ended.
            state::reset(&rcfg.state_dir, &session);
        }
        rootcheck::Judged::Waived => {}
        rootcheck::Judged::Blocked { reason, key } => {
            let mut reason = format!("{header}\n{reason}");
            // The shared reason text names `donegate skip`, which this hook does
            // not consume (see the module docs); say so rather than mislead.
            reason.push_str(
                "\n\n(At SubagentStop `donegate skip` is NOT consumed: it belongs to the parent \
                 session. This block is bounded by max_attempts and the identical-finding waiver.)",
            );
            let key = format!("{header}\n{key}");
            harness_core::repeat::emit_stop_block_keyed("donegate", &reason, &key);
            std::process::exit(0);
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

    /// Re-scoped by user rulings 2026-11-08 ("donegateのworktreeテストは問題
    /// ありすぎるので即時に停止"; "donegateそのものが他のworktreeを見たり検証したり
    /// テストしたりすることは禁止とする"). Was `owned_roots_include_the_cwd_only_when_written`
    /// (the subagent owned every written root); now only a write inside the cwd
    /// makes the cwd judged, and a write elsewhere selects nothing.
    #[test]
    fn writes_in_selects_the_cwd_only_when_written_inside_it() {
        let d = std::env::temp_dir().join(format!("dg-owned-{}", std::process::id()));
        let cwd = d.join("cwd");
        let wt = d.join("wt");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        assert!(!crate::rootcheck::writes_in(
            &cwd,
            &writes(&[&wt.join("f.rs")])
        ));
        assert!(crate::rootcheck::writes_in(
            &cwd,
            &writes(&[&cwd.join("f.rs")])
        ));
        // A gone directory inside the cwd is still inside the cwd.
        assert!(crate::rootcheck::writes_in(
            &cwd,
            &writes(&[&cwd.join("gone/f.rs")])
        ));
        // A relative path cannot be placed: the cwd is judged (restrictive).
        assert!(crate::rootcheck::writes_in(
            &cwd,
            &writes(&[Path::new("rel/f.rs")])
        ));
        let _ = std::fs::remove_dir_all(&d);
    }
}
