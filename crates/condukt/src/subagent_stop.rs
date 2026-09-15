//! `condukt subagent-stop` — the SubagentStop observability hook.
//!
//! When a child sub-agent (a condukt worker/verifier dispatched with the Task
//! tool) terminates, Claude Code fires `SubagentStop`. This module turns that
//! event into ONE durable fact: the owning run task's `updated_at` advances to
//! the moment the child stopped, which is the timestamp `condukt state probe`,
//! `condukt circuit check` and the claim reap gate already read when they ask
//! whether a worker is still making progress.
//!
//! # This writes a liveness timestamp, so "I do not know" must write NOTHING
//!
//! A progress timestamp written on a payload we could not attribute is a
//! fabricated heartbeat: it makes a dead or silent child indistinguishable from
//! a working one for every downstream TTL/idle judgement. So every step that can
//! fail to reach a conclusion resolves to "write nothing and say why" on stderr,
//! never to `now()`:
//!
//! * stdin empty or unparseable → nothing written
//! * `hook_event_name` is not `SubagentStop` → nothing written
//! * the project has no readable run state → nothing written
//! * the payload's `cwd` is not inside any task's recorded worktree → nothing
//!   written (the payload names no task of ours)
//! * two or more tasks match the same path → nothing written (ambiguous)
//! * the matched task is not `running` → nothing written
//!
//! [`resolve_owner`] returns [`Determination`] rather than `Option` so those
//! answers carry their reason, and [`record_progress`] re-checks `running`
//! while holding the run lock, so the state it writes against is the state it
//! judged.
//!
//! # What attribution actually rests on (stated without softening)
//!
//! The only field of a SubagentStop payload that ties it to a condukt task is
//! `cwd`, matched against `TaskState.worktree` recorded in this project's run
//! state. That means this hook advances a timestamp exactly when the stopping
//! sub-agent's working directory is (inside) a live task worktree. When the
//! child ran with the parent session's cwd instead — the main repo checkout —
//! nothing matches and nothing is written. That is a real coverage gap, not a
//! solved problem; it is reported on stderr every time rather than papered over
//! with a guess such as "there is only one running task, so it must be that
//! one".
//!
//! The hook returns no verdict — it observes, it does not gate — so the caller
//! runs it under `run_hook` (exit 0). "Never break the turn" governs the exit
//! code only; it is never a reason to write a timestamp we could not justify.

use crate::config::Config;
use crate::state::{self, RunState, Status};
use harness_core::verdict::{Determination, Required};
use std::path::{Path, PathBuf};

/// The run task a SubagentStop payload provably belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    pub run_id: String,
    pub task_id: String,
    /// The recorded worktree that matched the payload's `cwd`.
    pub worktree: PathBuf,
}

/// What one invocation did. Both arms are reported; there is no silent arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// `updated_at` of `task_id` in `run_id` was advanced to `at`.
    Touched {
        run_id: String,
        task_id: String,
        at: i64,
    },
    /// No timestamp was written, and this is why.
    NotWritten(String),
}

impl Outcome {
    /// Operator-facing line (stderr).
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Outcome::Touched {
                run_id,
                task_id,
                at,
            } => format!("advanced updated_at of task '{task_id}' in run '{run_id}' to {at}"),
            Outcome::NotWritten(why) => {
                format!("no progress timestamp written: {why}")
            }
        }
    }
}

/// Resolve which task of `runs` the payload's `cwd` belongs to. Pure: no
/// filesystem, no clock.
///
/// `Known` requires ALL of: a non-empty `cwd`; exactly one task across `runs`
/// whose recorded `worktree` is `cwd` or an ancestor of it; and that task being
/// `Status::Running`. Every other shape is `Undetermined` with the reason —
/// including the empty-`runs` case, which is "we could not find a run to
/// attribute this to", never "there is nothing to update".
///
/// This is the single place an unattributable payload is turned away; callers
/// reach the value only through [`Determination::require`], so there is no
/// method that quietly substitutes a default owner.
pub fn resolve_owner(payload_cwd: &str, runs: &[RunState]) -> Determination<Owner> {
    let cwd = payload_cwd.trim();
    if cwd.is_empty() {
        return Determination::undetermined(
            "subagent-stop: the payload carries no cwd, so the stopping sub-agent \
             cannot be attributed to a task",
        );
    }
    if runs.is_empty() {
        return Determination::undetermined(format!(
            "subagent-stop: no readable run state for this project, so cwd '{cwd}' \
             cannot be attributed to a task"
        ));
    }

    let cwd_path = PathBuf::from(cwd);
    let mut matches: Vec<(&RunState, &state::TaskState)> = Vec::new();
    for rs in runs {
        for t in &rs.tasks {
            let Some(wt) = &t.worktree else { continue };
            let wt_path = PathBuf::from(wt);
            if cwd_path == wt_path || cwd_path.starts_with(&wt_path) {
                matches.push((rs, t));
            }
        }
    }

    match matches.as_slice() {
        [] => Determination::undetermined(format!(
            "subagent-stop: cwd '{cwd}' is not inside any task worktree recorded in \
             this project's run state; the payload names no task of ours"
        )),
        [(rs, t)] => {
            if t.status != Status::Running {
                return Determination::undetermined(format!(
                    "subagent-stop: task '{}' in run '{}' owns cwd '{cwd}' but its status \
                     is {:?}, not running; a stopped sub-agent is no evidence that a \
                     non-running task is making progress",
                    t.id, rs.run_id, t.status
                ));
            }
            Determination::known(Owner {
                run_id: rs.run_id.clone(),
                task_id: t.id.clone(),
                worktree: PathBuf::from(t.worktree.clone().unwrap_or_default()),
            })
        }
        many => {
            let ids: Vec<String> = many
                .iter()
                .map(|(rs, t)| format!("{}/{}", rs.run_id, t.id))
                .collect();
            Determination::undetermined(format!(
                "subagent-stop: cwd '{cwd}' matches {} tasks ({}); which one stopped \
                 cannot be determined",
                ids.len(),
                ids.join(", ")
            ))
        }
    }
}

/// Advance `owner`'s `updated_at` to `now`, under the per-run state lock.
///
/// The `running` check is repeated here against the state loaded INSIDE the
/// lock: [`resolve_owner`] judged a snapshot, and the task may have settled
/// between that read and this write. A task that is no longer running gets no
/// timestamp. Returns the reason when nothing was written.
pub fn record_progress(cfg: &Config, state_cwd: &Path, owner: &Owner, now: i64) -> Outcome {
    let mut skipped: Option<String> = None;
    let res = state::with_run_locked(cfg, state_cwd, &owner.run_id, |rs| {
        match rs.tasks.iter_mut().find(|t| t.id == owner.task_id) {
            Some(t) if t.status == Status::Running => t.updated_at = Some(now),
            Some(t) => {
                skipped = Some(format!(
                    "task '{}' in run '{}' is {:?}, not running (it settled between \
                     resolution and the locked write)",
                    t.id, owner.run_id, t.status
                ));
            }
            None => {
                skipped = Some(format!(
                    "task '{}' is no longer present in run '{}'",
                    owner.task_id, owner.run_id
                ));
            }
        }
    });

    if let Err(e) = res {
        return Outcome::NotWritten(format!(
            "run '{}' state could not be updated ({e:#}); the stored timestamp is \
             unchanged",
            owner.run_id
        ));
    }
    match skipped {
        Some(why) => Outcome::NotWritten(why),
        None => Outcome::Touched {
            run_id: owner.run_id.clone(),
            task_id: owner.task_id.clone(),
            at: now,
        },
    }
}

/// The whole hook: a raw SubagentStop payload in, one [`Outcome`] out.
///
/// `state_cwd` is the directory the run state is addressed from (the hook
/// process's own cwd — the session's repo checkout), which is how every other
/// condukt run-state reader locates the project. The payload's `cwd` is used
/// only to attribute the stop to a task; it is deliberately NOT used to address
/// the state directory, because a linked worktree hashes to a different project
/// key than the repo the run was created in.
pub fn run(cfg: &Config, state_cwd: &Path, raw_stdin: &str, now: i64) -> Outcome {
    let Some(input) = harness_core::hook::HookInput::parse(raw_stdin) else {
        return Outcome::NotWritten(
            "stdin was empty or not a parseable hook payload; nothing was attributed".to_string(),
        );
    };
    if input.hook_event_name != "SubagentStop" {
        return Outcome::NotWritten(format!(
            "payload is a '{}' event, not SubagentStop; no sub-agent termination was \
             observed",
            input.hook_event_name
        ));
    }

    let runs = state::all_runs(cfg, state_cwd);
    match resolve_owner(&input.cwd, &runs).require() {
        Required::Blocked(v) => Outcome::NotWritten(
            v.reason()
                .map(|r| r.as_str().to_string())
                .unwrap_or_else(|| "undetermined owner".to_string()),
        ),
        Required::Determined(owner) => record_progress(cfg, state_cwd, &owner, now),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::TaskState;

    /// True only when a durable progress timestamp was actually written.
    /// Test-local on purpose: production reports through [`Outcome::describe`]
    /// and has no need for the predicate, so shipping it would be dead code.
    fn wrote(o: &Outcome) -> bool {
        matches!(o, Outcome::Touched { .. })
    }

    /// The reason an `Undetermined` carries, panicking (with what came back
    /// instead) when the determination was `Known`.
    ///
    /// Asserting on the REASON — not merely on "it was blocked" — is what makes
    /// an undetermined arm observable: several arms here return `Undetermined`
    /// for different causes, so a verdict-only assertion passes even when the
    /// specific guard under test has been deleted.
    fn undetermined_reason<T: std::fmt::Debug>(d: Determination<T>) -> String {
        match d.require() {
            Required::Determined(v) => {
                panic!("expected Undetermined, got Known({v:?})")
            }
            Required::Blocked(verdict) => verdict
                .reason()
                .map(|r| r.as_str().to_string())
                .expect("an Undetermined verdict always carries a reason"),
        }
    }

    fn test_cfg(tmp: &Path) -> Config {
        Config {
            worktree_base: tmp.join("worktrees"),
            default_branch: "main".to_string(),
            shared_globs: Vec::new(),
            max_parallel: 4,
            state_dir: tmp.to_path_buf(),
            test_command: None,
            stuck_ttl_secs: 1800,
            build_command: None,
            deploy_command: None,
            loop_max_iters: 10,
            autonomous: false,
            autonomy_source: harness_core::autonomy::Source::BuiltinDefault,
            consensus_enabled: false,
            consensus_samples: crate::consensus::DEFAULT_SAMPLES,
            consensus_threshold: crate::consensus::DEFAULT_THRESHOLD,
            adversarial_enabled: false,
            adversarial_size: crate::adversarial::DEFAULT_PANEL,
            adversarial_min_voters: crate::adversarial::DEFAULT_MIN_VOTERS,
            adversarial_block_ratio: crate::adversarial::DEFAULT_BLOCK_RATIO,
            single_worktree: false,
            worker_sandbox_enabled: false,
            worker_sandbox_image: None,
            worker_sandbox_memory: None,
            worker_sandbox_cpus: None,
            worker_sandbox_pids_limit: None,
        }
    }

    /// A private temp dir for one test, used both as the state dir and as the
    /// cwd the state is addressed from.
    fn tmpdir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "condukt-subagent-stop-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&p).expect("create temp dir");
        p
    }

    fn task(
        id: &str,
        status: Status,
        worktree: Option<&str>,
        updated_at: Option<i64>,
    ) -> TaskState {
        TaskState {
            id: id.to_string(),
            status,
            worktree: worktree.map(|s| s.to_string()),
            updated_at,
            ..Default::default()
        }
    }

    fn run_with(run_id: &str, tasks: Vec<TaskState>) -> RunState {
        RunState {
            run_id: run_id.to_string(),
            goal: "g".to_string(),
            tasks,
            paused: false,
            terminal_label: None,
            recorded_at: None,
        }
    }

    fn payload(event: &str, cwd: &str) -> String {
        format!(
            r#"{{"session_id":"s","transcript_path":"/tmp/t.jsonl","cwd":"{cwd}","hook_event_name":"{event}"}}"#
        )
    }

    /// Read back the stored `updated_at` of a task straight from disk, so the
    /// assertions are about the DURABLE timestamp, not an in-memory value.
    fn stored_updated_at(cfg: &Config, cwd: &Path, run_id: &str, task_id: &str) -> Option<i64> {
        let rs = RunState::load(cfg, cwd, run_id).expect("run state loads");
        rs.tasks
            .iter()
            .find(|t| t.id == task_id)
            .expect("task present")
            .updated_at
    }

    /// A well-formed SubagentStop payload whose cwd is a running task's
    /// worktree advances that task's DURABLE `updated_at` to the supplied
    /// moment — the timestamp `state probe` / `circuit check` measure the TTL
    /// against.
    #[test]
    fn wellformed_payload_advances_durable_updated_at() {
        let tmp = tmpdir("advance");
        let cfg = test_cfg(&tmp);
        let wt = tmp.join("wt-t1");
        std::fs::create_dir_all(&wt).unwrap();

        let rs = run_with(
            "run-1",
            vec![task(
                "t1",
                Status::Running,
                Some(wt.to_str().unwrap()),
                Some(1_000),
            )],
        );
        rs.save(&cfg, &tmp).expect("save run state");

        let out = run(
            &cfg,
            &tmp,
            &payload("SubagentStop", wt.to_str().unwrap()),
            9_999,
        );

        assert!(
            wrote(&out),
            "a well-formed payload for a running task must write: {}",
            out.describe()
        );
        assert_eq!(
            out,
            Outcome::Touched {
                run_id: "run-1".to_string(),
                task_id: "t1".to_string(),
                at: 9_999,
            }
        );
        assert_eq!(
            stored_updated_at(&cfg, &tmp, "run-1", "t1"),
            Some(9_999),
            "the stored timestamp must advance to the moment the sub-agent stopped"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A payload that names no task of ours must leave the stored timestamp
    /// EXACTLY as it was. This is the fail-closed half: a fabricated heartbeat
    /// here would make a dead child look alive to every TTL consumer.
    #[test]
    fn undetermined_payload_does_not_advance_updated_at() {
        let tmp = tmpdir("undetermined");
        let cfg = test_cfg(&tmp);
        let wt = tmp.join("wt-t1");
        std::fs::create_dir_all(&wt).unwrap();

        let rs = run_with(
            "run-1",
            vec![task(
                "t1",
                Status::Running,
                Some(wt.to_str().unwrap()),
                Some(1_000),
            )],
        );
        rs.save(&cfg, &tmp).expect("save run state");

        // cwd is the repo checkout, not any recorded worktree: unattributable.
        let elsewhere = tmp.join("somewhere-else");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let out = run(
            &cfg,
            &tmp,
            &payload("SubagentStop", elsewhere.to_str().unwrap()),
            9_999,
        );

        assert!(
            !wrote(&out),
            "an unattributable payload must not write a progress timestamp: {}",
            out.describe()
        );
        assert!(
            matches!(&out, Outcome::NotWritten(why) if why.contains("names no task of ours")),
            "the reason must say why nothing was written, got: {}",
            out.describe()
        );
        assert_eq!(
            stored_updated_at(&cfg, &tmp, "run-1", "t1"),
            Some(1_000),
            "the stored timestamp must be untouched by an undetermined payload"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Unparseable stdin is a cannot-determine, not a heartbeat.
    #[test]
    fn unparseable_stdin_writes_nothing() {
        let tmp = tmpdir("unparseable");
        let cfg = test_cfg(&tmp);
        let wt = tmp.join("wt-t1");
        let rs = run_with(
            "run-1",
            vec![task(
                "t1",
                Status::Running,
                Some(wt.to_str().unwrap()),
                Some(1_000),
            )],
        );
        rs.save(&cfg, &tmp).expect("save run state");

        for raw in ["", "   ", "{not json", r#"{"cwd":"#] {
            let out = run(&cfg, &tmp, raw, 9_999);
            assert!(!wrote(&out), "raw {raw:?} must not write");
        }
        assert_eq!(stored_updated_at(&cfg, &tmp, "run-1", "t1"), Some(1_000));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A non-SubagentStop payload (e.g. a Stop event delivered to this
    /// subcommand) is not evidence a sub-agent terminated.
    #[test]
    fn wrong_event_name_writes_nothing() {
        let tmp = tmpdir("wrongevent");
        let cfg = test_cfg(&tmp);
        let wt = tmp.join("wt-t1");
        std::fs::create_dir_all(&wt).unwrap();
        let rs = run_with(
            "run-1",
            vec![task(
                "t1",
                Status::Running,
                Some(wt.to_str().unwrap()),
                Some(1_000),
            )],
        );
        rs.save(&cfg, &tmp).expect("save run state");

        let out = run(&cfg, &tmp, &payload("Stop", wt.to_str().unwrap()), 9_999);
        assert!(!wrote(&out), "{}", out.describe());
        assert_eq!(stored_updated_at(&cfg, &tmp, "run-1", "t1"), Some(1_000));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The owning task exists and owns the cwd, but has already settled. A
    /// stopped sub-agent is no evidence that a non-running task is progressing.
    ///
    /// The body asserts BOTH halves its name claims: that resolution itself is
    /// `Undetermined` (not merely that no write happened downstream — two
    /// separate guards can produce that same outcome), and that the stored
    /// timestamp is untouched end-to-end.
    #[test]
    fn non_running_owner_is_undetermined_and_writes_nothing() {
        let tmp = tmpdir("settled");
        let cfg = test_cfg(&tmp);
        let wt = tmp.join("wt-t1");
        std::fs::create_dir_all(&wt).unwrap();
        let rs = run_with(
            "run-1",
            vec![
                task("t1", Status::Done, Some(wt.to_str().unwrap()), Some(1_000)),
                task("t2", Status::Running, None, Some(1_000)),
            ],
        );
        rs.save(&cfg, &tmp).expect("save run state");

        let out = run(
            &cfg,
            &tmp,
            &payload("SubagentStop", wt.to_str().unwrap()),
            9_999,
        );
        assert!(!wrote(&out), "{}", out.describe());
        assert_eq!(stored_updated_at(&cfg, &tmp, "run-1", "t1"), Some(1_000));

        // The half the name claims and the end-to-end assertions above cannot
        // see: resolution ITSELF refused, naming the status it observed.
        let why = undetermined_reason(resolve_owner(wt.to_str().unwrap(), &[rs]));
        assert!(
            why.contains("not running") && why.contains("Done"),
            "resolution must refuse a settled owner and name its status; got: {why}"
        );
        // And the run's other running task was not stamped in its place.
        assert_eq!(stored_updated_at(&cfg, &tmp, "run-1", "t2"), Some(1_000));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// No run state at all for the project: an empty set is "we found nothing
    /// to attribute this to", never a licence to write.
    #[test]
    fn no_run_state_writes_nothing() {
        let tmp = tmpdir("noruns");
        let cfg = test_cfg(&tmp);
        let out = run(
            &cfg,
            &tmp,
            &payload("SubagentStop", tmp.to_str().unwrap()),
            9_999,
        );
        assert!(!wrote(&out), "{}", out.describe());
        assert!(
            matches!(&out, Outcome::NotWritten(why) if why.contains("no readable run state")),
            "got: {}",
            out.describe()
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Two tasks recording the same worktree: which one stopped cannot be
    /// determined, so neither is touched.
    #[test]
    fn ambiguous_owner_writes_nothing() {
        let tmp = tmpdir("ambiguous");
        let cfg = test_cfg(&tmp);
        let wt = tmp.join("shared-wt");
        std::fs::create_dir_all(&wt).unwrap();
        let rs = run_with(
            "run-1",
            vec![
                task(
                    "t1",
                    Status::Running,
                    Some(wt.to_str().unwrap()),
                    Some(1_000),
                ),
                task(
                    "t2",
                    Status::Running,
                    Some(wt.to_str().unwrap()),
                    Some(1_000),
                ),
            ],
        );
        rs.save(&cfg, &tmp).expect("save run state");

        let out = run(
            &cfg,
            &tmp,
            &payload("SubagentStop", wt.to_str().unwrap()),
            9_999,
        );
        assert!(!wrote(&out), "{}", out.describe());
        assert_eq!(stored_updated_at(&cfg, &tmp, "run-1", "t1"), Some(1_000));
        assert_eq!(stored_updated_at(&cfg, &tmp, "run-1", "t2"), Some(1_000));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A cwd BELOW the worktree root (the usual case: the child cd'd into a
    /// subdirectory) still resolves to the owning task.
    #[test]
    fn cwd_below_worktree_resolves_to_owner() {
        let wt = "/tmp/wt-a";
        let runs = vec![run_with(
            "run-1",
            vec![task("t1", Status::Running, Some(wt), Some(1_000))],
        )];
        match resolve_owner("/tmp/wt-a/crates/condukt", &runs).require() {
            Required::Determined(o) => {
                assert_eq!(o.run_id, "run-1");
                assert_eq!(o.task_id, "t1");
            }
            Required::Blocked(v) => panic!("must resolve: {:?}", v.reason()),
        }
    }

    /// A sibling path that merely shares a textual prefix with the worktree
    /// (`/tmp/wt-a-other` vs `/tmp/wt-a`) is NOT inside it.
    #[test]
    fn sibling_prefix_path_is_not_inside_the_worktree() {
        let runs = vec![run_with(
            "run-1",
            vec![task("t1", Status::Running, Some("/tmp/wt-a"), Some(1_000))],
        )];
        assert!(
            matches!(
                resolve_owner("/tmp/wt-a-other", &runs).require(),
                Required::Blocked(_)
            ),
            "a sibling directory must not be attributed to the task"
        );
    }

    /// An empty cwd carries no attribution at all — and says SO, naming the
    /// missing cwd rather than the downstream "no worktree matched".
    ///
    /// The reason is asserted, not just the verdict: an empty cwd already falls
    /// through to an empty `matches` and comes back `Undetermined` from the `[]`
    /// arm, so a verdict-only assertion cannot observe the dedicated guard at
    /// all (it survived deletion with the suite green). The two answers differ
    /// only in what the operator is told, which is the whole product of an
    /// undetermined arm, so that is what this pins.
    #[test]
    fn empty_cwd_is_undetermined_and_says_the_cwd_is_missing() {
        let runs = vec![run_with(
            "run-1",
            vec![task("t1", Status::Running, Some("/tmp/wt-a"), Some(1_000))],
        )];
        let why = undetermined_reason(resolve_owner("   ", &runs));
        assert!(
            why.contains("carries no cwd"),
            "an empty cwd must be diagnosed as a MISSING cwd, not as an \
             unmatched worktree; got: {why}"
        );
    }

    /// The `[(rs, t)]` arm's own `running` check, observed directly rather than
    /// through `run()`.
    ///
    /// `resolve_owner` and `record_progress` each guard the same fact, so an
    /// end-to-end assertion cannot tell which one refused: deleting either left
    /// the other to produce an identical observable outcome, and both deletions
    /// survived a green suite. `resolve_owner` is pure, so this calls it
    /// directly and asserts the DETERMINATION and the status it names.
    #[test]
    fn resolve_owner_refuses_a_settled_owner_and_names_its_status() {
        let runs = vec![run_with(
            "run-1",
            vec![task("t1", Status::Done, Some("/tmp/wt-a"), Some(1_000))],
        )];
        let why = undetermined_reason(resolve_owner("/tmp/wt-a", &runs));
        assert!(
            why.contains("not running"),
            "the refusal must say the task is not running; got: {why}"
        );
        assert!(
            why.contains("Done"),
            "the refusal must name the status observed; got: {why}"
        );
    }

    /// Every non-running status is refused, not just the one sampled above —
    /// `Running` is the only status that may receive a heartbeat.
    #[test]
    fn only_a_running_task_can_be_resolved_as_owner() {
        for status in [
            Status::Pending,
            Status::Done,
            Status::Failed,
            Status::Verified,
            Status::Cancelled,
            Status::Discarded,
        ] {
            let runs = vec![run_with(
                "run-1",
                vec![task("t1", status, Some("/tmp/wt-a"), Some(1_000))],
            )];
            let why = undetermined_reason(resolve_owner("/tmp/wt-a", &runs));
            assert!(
                why.contains("not running"),
                "status {status:?} must not resolve to an owner; got: {why}"
            );
        }
        // The positive control: the same input with `Running` DOES resolve, so
        // the loop above is discriminating on status and not on some other
        // property of the fixture.
        let runs = vec![run_with(
            "run-1",
            vec![task("t1", Status::Running, Some("/tmp/wt-a"), Some(1_000))],
        )];
        assert!(matches!(
            resolve_owner("/tmp/wt-a", &runs).require(),
            Required::Determined(_)
        ));
    }

    /// `record_progress`'s OWN `running` re-check, called directly with a
    /// hand-built `Owner` — the "it settled between resolution and the locked
    /// write" race the function's docstring claims to handle.
    ///
    /// Nothing else can reach this path: `run()` cannot produce an `Owner` for a
    /// settled task, because `resolve_owner` refuses one first. So without this
    /// test the documented race is unverified and the guard has no kill rate.
    #[test]
    fn record_progress_refuses_a_task_that_settled_after_resolution() {
        let tmp = tmpdir("settled-race");
        let cfg = test_cfg(&tmp);
        let wt = tmp.join("wt-t1");
        std::fs::create_dir_all(&wt).unwrap();
        let rs = run_with(
            "run-1",
            vec![task(
                "t1",
                Status::Done,
                Some(wt.to_str().unwrap()),
                Some(1_000),
            )],
        );
        rs.save(&cfg, &tmp).expect("save run state");

        // The Owner a resolution made a moment earlier, when the task was still
        // running; the stored state has since settled.
        let owner = Owner {
            run_id: "run-1".to_string(),
            task_id: "t1".to_string(),
            worktree: wt.clone(),
        };
        let out = record_progress(&cfg, &tmp, &owner, 9_999);

        assert!(
            !wrote(&out),
            "a task that settled before the locked write must not be stamped: {}",
            out.describe()
        );
        assert!(
            matches!(&out, Outcome::NotWritten(why) if why.contains("settled between")),
            "the reason must name the settle-after-resolution race; got: {}",
            out.describe()
        );
        assert_eq!(
            stored_updated_at(&cfg, &tmp, "run-1", "t1"),
            Some(1_000),
            "the stored timestamp must be untouched"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The same re-check, in the positive direction: a still-running task IS
    /// stamped by a direct `record_progress` call. Without this, the test above
    /// would also pass if `record_progress` had simply stopped writing at all.
    #[test]
    fn record_progress_stamps_a_still_running_task() {
        let tmp = tmpdir("race-control");
        let cfg = test_cfg(&tmp);
        let wt = tmp.join("wt-t1");
        std::fs::create_dir_all(&wt).unwrap();
        let rs = run_with(
            "run-1",
            vec![task(
                "t1",
                Status::Running,
                Some(wt.to_str().unwrap()),
                Some(1_000),
            )],
        );
        rs.save(&cfg, &tmp).expect("save run state");

        let owner = Owner {
            run_id: "run-1".to_string(),
            task_id: "t1".to_string(),
            worktree: wt.clone(),
        };
        let out = record_progress(&cfg, &tmp, &owner, 9_999);

        assert!(wrote(&out), "{}", out.describe());
        assert_eq!(stored_updated_at(&cfg, &tmp, "run-1", "t1"), Some(9_999));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A task named by an `Owner` but absent from the run gets no timestamp,
    /// and the run's other tasks are not stamped in its place.
    #[test]
    fn record_progress_refuses_a_task_that_is_gone() {
        let tmp = tmpdir("gone");
        let cfg = test_cfg(&tmp);
        let rs = run_with(
            "run-1",
            vec![task("t1", Status::Running, Some("/tmp/wt-a"), Some(1_000))],
        );
        rs.save(&cfg, &tmp).expect("save run state");

        let owner = Owner {
            run_id: "run-1".to_string(),
            task_id: "vanished".to_string(),
            worktree: PathBuf::from("/tmp/wt-a"),
        };
        let out = record_progress(&cfg, &tmp, &owner, 9_999);

        assert!(!wrote(&out), "{}", out.describe());
        assert!(
            matches!(&out, Outcome::NotWritten(why) if why.contains("no longer present")),
            "got: {}",
            out.describe()
        );
        assert_eq!(stored_updated_at(&cfg, &tmp, "run-1", "t1"), Some(1_000));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
