//! Roots the parent's Stop leaves to a still-running background subagent
//! (backlog 95deaa32).
//!
//! `harness_core::session_roots::resolve` makes every root a subagent sidechain
//! wrote into a root of the PARENT's Stop. While that subagent is still running
//! in the background, judging its root races its in-flight edits and reports
//! mid-edit state to an agent that does not own the tree. Such a root is
//! instead judged at the owner's own `SubagentStop` ([`crate::subagent`]), and
//! again at the parent's Stop once the owner has finished.
//!
//! A root is deferred only when ALL of these are observed; anything else keeps
//! today's behaviour (the parent's Stop judges it):
//!
//! * the hook cwd root is never deferred;
//! * every transcript (parent and each sidechain) can be read, and the sidechain
//!   directory can be listed;
//! * at least one sidechain that wrote the root is IN FLIGHT ([`in_flight`]);
//! * no other transcript — the parent, or a sidechain that is not observed to be
//!   in flight (finished OR undeterminable) — wrote the root, judged with the
//!   same write-to-root mapping as the roots themselves
//!   (`session_roots::roots_of_writes`), plus any Bash call whose cwd lies in
//!   the root. If any of those transcripts' roots cannot be determined, nothing
//!   is deferred.
//!
//! The caller names every deferred root in its output (never a silent skip).

use std::path::{Path, PathBuf};

use harness_core::verdict::Determination;

/// A root the parent's Stop does not judge, and the in-flight agents that
/// alone wrote it.
pub(crate) struct Deferred {
    pub(crate) root: PathBuf,
    pub(crate) agents: Vec<String>,
}

impl Deferred {
    /// The operator/model-facing line naming the root.
    pub(crate) fn notice(&self) -> String {
        format!(
            "[root: {}] NOT judged at this stop: only background subagent(s) {} wrote it and \
             they are still running. donegate judges it at their SubagentStop, and here again \
             once they have finished.",
            self.root.display(),
            self.agents.join(", ")
        )
    }
}

/// `agent-<id>.jsonl` -> `<id>`.
pub(crate) fn agent_id_of(sidechain: &Path) -> Option<String> {
    let stem = sidechain.file_stem()?.to_str()?;
    let id = stem.strip_prefix("agent-")?;
    (!id.is_empty()).then(|| id.to_string())
}

/// Whether the agent of `sidechain` (id `agent_id`) is observed to be still
/// running in the background, searching `transcripts` (the text of the parent
/// and every sidechain — a nested agent's launch record and its completion
/// notification sit in its launcher's sidechain, not in the parent).
///
/// * `Known(false)` — finished: some transcript holds a line naming the agent
///   together with `task-id` (a `<task-notification>`), or a `toolUseResult`
///   for it whose status is not `async_launched` (a synchronous run).
/// * `Known(true)` — launched in the background and not finished:
///   `agent-<id>.meta.json` names `toolUseId`, and a transcript holds the
///   matching tool_result with `toolUseResult {isAsync: true, status:
///   "async_launched", agentId: <id>}`.
/// * `Undetermined` — anything else (no / unreadable meta, no launch record).
///
/// The finished-test is deliberately loose: a false "finished" only makes the
/// parent judge the root, which is the restrictive side.
pub(crate) fn in_flight(
    agent_id: &str,
    sidechain: &Path,
    transcripts: &[String],
) -> Determination<bool> {
    let mut launched_async = false;
    let mut tool_use_ids: Vec<String> = Vec::new();
    for text in transcripts {
        for line in text.lines().filter(|l| l.contains(agent_id)) {
            if line.contains("task-id") {
                return Determination::known(false);
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let Some(tur) = v.get("toolUseResult") else {
                continue;
            };
            if tur.get("agentId").and_then(|a| a.as_str()) != Some(agent_id) {
                continue;
            }
            let is_async = tur.get("isAsync").and_then(|a| a.as_bool()) == Some(true);
            let status = tur.get("status").and_then(|s| s.as_str());
            if !(is_async && status == Some("async_launched")) {
                return Determination::known(false);
            }
            launched_async = true;
            if let Some(items) = v.pointer("/message/content").and_then(|c| c.as_array()) {
                tool_use_ids.extend(
                    items
                        .iter()
                        .filter_map(|i| i.get("tool_use_id").and_then(|t| t.as_str()))
                        .map(String::from),
                );
            }
        }
    }
    let meta = sidechain.with_extension("meta.json");
    let tool_use_id = match std::fs::read_to_string(&meta)
        .map_err(|e| e.to_string())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).map_err(|e| e.to_string()))
    {
        Ok(v) => match v.get("toolUseId").and_then(|t| t.as_str()) {
            Some(t) if !t.is_empty() => t.to_string(),
            _ => {
                return Determination::undetermined(format!(
                    "{} names no toolUseId",
                    meta.display()
                ))
            }
        },
        Err(e) => {
            return Determination::undetermined(format!("cannot read {}: {e}", meta.display()))
        }
    };
    if launched_async && tool_use_ids.contains(&tool_use_id) {
        Determination::known(true)
    } else {
        Determination::undetermined(format!(
            "no background launch record of agent {agent_id} (tool_use {tool_use_id}) was found"
        ))
    }
}

/// The roots of ONE transcript's writes (index 0 is the hook cwd, judged
/// anyway), plus every root holding the cwd of one of its Bash calls.
fn roots_written_by(
    roots: &[PathBuf],
    transcript: &Path,
    session_transcript: &str,
    store: &Path,
) -> Determination<Vec<PathBuf>> {
    let t = transcript.to_string_lossy();
    let w = match harness_core::attribution::own_written_paths(&t) {
        Determination::Known(w) => w,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let sr = match harness_core::session_roots::roots_of_writes(
        &roots[0],
        &w,
        session_transcript,
        store,
    ) {
        Determination::Known(sr) => sr,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let mut out = sr.roots;
    let calls = match harness_core::transcript::bash_commands_by_session(&t) {
        Determination::Known(c) => c,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    for c in calls.iter().filter_map(|c| c.cwd.as_deref()) {
        let c = harness_core::attribution::resolve(Path::new(c));
        out.extend(roots.iter().filter(|r| c.starts_with(r)).cloned());
    }
    Determination::known(out)
}

/// Split the parent's `roots` into the ones its Stop judges and the notices
/// naming each deferred root ([`deferred_roots`]). When the deferral cannot be
/// determined, EVERY root is kept and the one notice says why.
pub(crate) fn split(roots: Vec<PathBuf>, transcript_path: &str) -> (Vec<PathBuf>, Vec<String>) {
    match deferred_roots(&roots, transcript_path) {
        Determination::Known(d) => {
            let notices = d.iter().map(Deferred::notice).collect();
            let kept = roots
                .into_iter()
                .filter(|r| !d.iter().any(|x| &x.root == r))
                .collect();
            (kept, notices)
        }
        Determination::Undetermined(why) => (
            roots,
            vec![format!(
                "in-flight background subagent roots could not be determined ({}); every \
                 root was judged at this stop",
                why.as_str()
            )],
        ),
    }
}

/// The roots (from `roots`, cwd first) the parent's Stop must not judge
/// because only in-flight background subagents wrote them.
///
/// `Known(empty)` is an observation (a single root, no transcript, no
/// sidechain, or no sidechain observed in flight). `Undetermined` means a
/// transcript, the sidechain listing, or a non-running writer's roots could
/// not be read; the caller then defers NOTHING (every root is judged) and
/// reports why. A per-agent liveness the module cannot establish is not an
/// error here: that agent simply counts as not running (see the module docs).
pub(crate) fn deferred_roots(
    roots: &[PathBuf],
    transcript_path: &str,
) -> Determination<Vec<Deferred>> {
    if roots.len() < 2 || transcript_path.trim().is_empty() {
        return Determination::known(Vec::new());
    }
    let parent = PathBuf::from(transcript_path);
    let sides = match harness_core::transcript::subagent_transcripts_strict(&parent) {
        Determination::Known(s) => s,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    if sides.is_empty() {
        return Determination::known(Vec::new());
    }
    let mut texts = Vec::with_capacity(sides.len() + 1);
    for p in std::iter::once(&parent).chain(sides.iter()) {
        match std::fs::read(p) {
            Ok(b) => texts.push(String::from_utf8_lossy(&b).into_owned()),
            Err(e) => {
                return Determination::undetermined(format!("cannot read {}: {e}", p.display()))
            }
        }
    }
    let mut live: Vec<(String, &PathBuf)> = Vec::new();
    let mut others: Vec<&PathBuf> = vec![&parent];
    for s in &sides {
        match agent_id_of(s) {
            Some(id) if matches!(in_flight(&id, s, &texts), Determination::Known(true)) => {
                live.push((id, s));
            }
            _ => others.push(s),
        }
    }
    if live.is_empty() {
        return Determination::known(Vec::new());
    }
    let store = harness_core::worktree_origin::default_store();
    let mut held: Vec<PathBuf> = Vec::new();
    for t in others {
        match roots_written_by(roots, t, transcript_path, &store) {
            Determination::Known(rs) => held.extend(rs),
            Determination::Undetermined(why) => {
                return Determination::undetermined(format!(
                    "the roots {} wrote cannot be determined: {}",
                    t.display(),
                    why.as_str()
                ))
            }
        }
    }
    // A live agent whose own roots cannot be determined owns nothing here, so
    // a root only it could have produced stays judged.
    let live_roots: Vec<(String, Option<Vec<PathBuf>>)> = live
        .into_iter()
        .map(
            |(id, s)| match roots_written_by(roots, s, transcript_path, &store) {
                Determination::Known(rs) => (id, Some(rs)),
                Determination::Undetermined(_) => (id, None),
            },
        )
        .collect();
    let mut out = Vec::new();
    for r in &roots[1..] {
        if held.contains(r) {
            continue;
        }
        let agents: Vec<String> = live_roots
            .iter()
            .filter(|(_, rs)| rs.as_ref().is_some_and(|rs| rs.contains(r)))
            .map(|(id, _)| id.clone())
            .collect();
        if !agents.is_empty() {
            out.push(Deferred {
                root: r.clone(),
                agents,
            });
        }
    }
    Determination::known(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dg-inflight-unit-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn launch(agent: &str, tool_use: &str, status: &str, is_async: bool) -> String {
        serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": [
                {"tool_use_id": tool_use, "type": "tool_result", "content": "x"}]},
            "toolUseResult": {"isAsync": is_async, "status": status, "agentId": agent}
        })
        .to_string()
    }

    fn sidechain(dir: &Path, agent: &str, tool_use: Option<&str>) -> PathBuf {
        let s = dir.join(format!("agent-{agent}.jsonl"));
        std::fs::write(&s, "").unwrap();
        if let Some(t) = tool_use {
            std::fs::write(
                dir.join(format!("agent-{agent}.meta.json")),
                serde_json::json!({"toolUseId": t}).to_string(),
            )
            .unwrap();
        }
        s
    }

    #[test]
    fn async_launch_without_notification_is_in_flight() {
        let d = tmp("live");
        let s = sidechain(&d, "a1", Some("tu1"));
        let texts = vec![launch("a1", "tu1", "async_launched", true)];
        assert!(matches!(
            in_flight("a1", &s, &texts),
            Determination::Known(true)
        ));
    }

    #[test]
    fn a_task_notification_in_any_transcript_means_finished() {
        let d = tmp("notified");
        let s = sidechain(&d, "a1", Some("tu1"));
        let texts = vec![
            launch("a1", "tu1", "async_launched", true),
            "{\"content\":\"<task-notification>\\n<task-id>a1</task-id>\"}".to_string(),
        ];
        assert!(matches!(
            in_flight("a1", &s, &texts),
            Determination::Known(false)
        ));
    }

    #[test]
    fn a_synchronous_result_means_finished() {
        let d = tmp("sync");
        let s = sidechain(&d, "a1", Some("tu1"));
        let texts = vec![launch("a1", "tu1", "completed", false)];
        assert!(matches!(
            in_flight("a1", &s, &texts),
            Determination::Known(false)
        ));
    }

    #[test]
    fn missing_meta_or_launch_or_mismatched_tool_use_is_undetermined() {
        let d = tmp("undet");
        let no_meta = sidechain(&d, "a1", None);
        let texts = vec![launch("a1", "tu1", "async_launched", true)];
        assert!(matches!(
            in_flight("a1", &no_meta, &texts),
            Determination::Undetermined(_)
        ));
        let s = sidechain(&d, "a2", Some("tu2"));
        assert!(matches!(
            in_flight("a2", &s, &[]),
            Determination::Undetermined(_)
        ));
        let other = vec![launch("a2", "tu-other", "async_launched", true)];
        assert!(matches!(
            in_flight("a2", &s, &other),
            Determination::Undetermined(_)
        ));
    }

    #[test]
    fn agent_id_comes_from_the_sidechain_file_name() {
        assert_eq!(
            agent_id_of(Path::new("/x/agent-ab12.jsonl")).as_deref(),
            Some("ab12")
        );
        assert_eq!(agent_id_of(Path::new("/x/agent-.jsonl")), None);
        assert_eq!(agent_id_of(Path::new("/x/other.jsonl")), None);
    }
}
