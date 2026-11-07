//! Run liveness for the runtime-overlap merge-hold gate ([`crate::worktree`]):
//! [`run_liveness`] and its three-valued [`RunLiveness`] verdict. Split out of
//! `claim.rs`; it reads the parent module's claim registry
//! ([`super::read_registry`]) plus the run's own state file.

use super::{is_stale, read_registry, registry_path, ttl_secs};
use crate::config::Config;
use harness_core::verdict::Determination;
use std::path::Path;

/// Three-valued liveness verdict for a run id in the claim registry — see
/// [`run_liveness`]. The third state ([`RunLiveness::Undetermined`]) is what
/// keeps the merge-hold gate from silently failing open: it lets the caller
/// tell "positively dead" apart from "could not find out", and fail CLOSED on
/// the latter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunLiveness {
    /// `run_id` holds ≥1 claim whose heartbeat is within the stuck-TTL, OR its
    /// run-state file exists and still has a non-terminal task (backlog
    /// 4fa94cba: a live run may hold zero claims).
    Live,
    /// POSITIVE evidence of death on both signals: the registry was read
    /// cleanly and holds no fresh claim for `run_id` (a genuinely-absent
    /// registry = no claims), AND the run's state file is either genuinely
    /// absent or read cleanly with every task terminal
    /// (verified/cancelled/discarded).
    Dead,
    /// Liveness could not be established (unreadable/corrupt registry, an
    /// unreadable/corrupt run-state file, an unresolvable project root, or an
    /// empty/unattributed `run_id`). Callers that gate on death MUST treat this
    /// as "still live" (fail closed).
    Undetermined,
}

/// Liveness of `run_id` in this project's claim registry, for the runtime-overlap
/// merge-hold gate ([`crate::worktree`]).
///
/// This is condukt's OWN liveness signal, in condukt's OWN run-id space: a live
/// run refreshes the heartbeat of the claims it holds (`state set --status
/// running` claims a task's files under the run id; every later `state set` /
/// `state heartbeat` refreshes them via [`super::heartbeat`]), so a claim with a fresh
/// heartbeat proves its run is alive. A run that has since crashed/been
/// abandoned stops heart-beating and its claims age out past the TTL
/// ([`is_stale`]) — exactly the same staleness the reaper uses. The hold's
/// `run_id` IS a condukt run id, so it shares this registry's identity space —
/// the whole point of keying liveness here rather than off a disjoint id space.
///
/// The gate uses this to decide whether a hold is still authoritative: a hold
/// placed by a LIVE run keeps blocking the merge, while a hold left by a DEAD
/// run ages out so it does not block a live task that merely REUSES the same
/// `condukt/<task.id>` branch name.
///
/// **"No claims" is not "dead" (backlog 4fa94cba).** A live run can hold ZERO
/// claims — its tasks' claims were all skipped by the cross-session claim gate,
/// its decomposition declared no `touched_files` (claims key off declared files
/// while holds are placed off the actual git diff), or it released its claims
/// at `verified` while another task is still open. So when the registry shows
/// no fresh claim, the run's OWN state file is consulted as a second signal: a
/// run-state file that exists and still has a non-terminal task (anything but
/// verified/cancelled/discarded — the same "open run" test as
/// [`crate::state::open_runs`]) is [`RunLiveness::Live`].
///
/// **Fail-closed contract.** [`RunLiveness::Dead`] requires positive evidence on
/// BOTH signals: a CLEAN registry read that finds no fresh claim, AND a run-state
/// file that is genuinely absent (ENOENT) or read cleanly with every task
/// terminal. A present-but-unreadable/corrupt registry, a run-state file that
/// cannot be located/read/parsed, or an empty `run_id` (an unattributed hold with
/// no placer to test), returns [`RunLiveness::Undetermined`] — never `Dead`. Collapsing "cannot read
/// the registry" into "the placer is dead, drop the hold" would re-enter the
/// reverted disjoint-id bug's failure class (an empty read making every hold
/// look dead → the gate silently off) one layer down; the caller must fail
/// closed on `Undetermined`.
///
/// A pure, lock-free read: it never persists (no reap-compaction) and never
/// takes the claims lock, so a merge liveness probe cannot contend with a live
/// run's claim writes. The atomic-rename saves mean this always observes a
/// COMPLETE registry (never a half-written one), so a torn read cannot
/// masquerade as a clean empty registry.
pub fn run_liveness(cfg: &Config, cwd: &Path, run_id: &str, now: i64) -> RunLiveness {
    if run_id.is_empty() {
        // No placer id to test — death cannot be established. Fail closed.
        return RunLiveness::Undetermined;
    }
    // No resolvable registry ⇒ liveness cannot be established. Fail closed:
    // `Undetermined` keeps the merge hold, exactly as a corrupt registry does.
    let path = match registry_path(cfg, cwd) {
        Determination::Known(p) => p,
        Determination::Undetermined(_) => return RunLiveness::Undetermined,
    };
    let fresh_claim = match read_registry(&path) {
        Ok(Some(reg)) => {
            let ttl = ttl_secs(cfg);
            reg.files
                .values()
                .chain(reg.task_claims.values())
                .any(|c| c.run_id == run_id && !is_stale(c, now, ttl))
        }
        // Genuinely-absent registry: no claims exist for anyone. That alone is
        // NOT death (backlog 4fa94cba) — the run state below decides.
        Ok(None) => false,
        // Present but unreadable/corrupt: liveness UNKNOWN. Fail closed.
        Err(_) => return RunLiveness::Undetermined,
    };
    if fresh_claim {
        return RunLiveness::Live;
    }
    // No fresh claim: only a run state that is positively absent or positively
    // terminal lets the run read as Dead.
    match run_state_open(cfg, cwd, run_id) {
        Determination::Known(true) => RunLiveness::Live,
        Determination::Known(false) => RunLiveness::Dead,
        Determination::Undetermined(_) => RunLiveness::Undetermined,
    }
}

/// Whether `run_id`'s run-state file exists and still has a non-terminal task,
/// as a three-valued observation for [`run_liveness`]:
///
/// * `Known(true)` — the file exists, parses, and some task is not
///   verified/cancelled/discarded (the [`crate::state::open_runs`] test);
/// * `Known(false)` — the file is genuinely absent (ENOENT), or parses with
///   every task terminal (a run with no tasks counts as terminal, exactly as in
///   `open_runs`);
/// * `Undetermined` — the path cannot be resolved (unresolvable project root,
///   an existence probe that errors), or the file exists but cannot be read or
///   parsed. Never folded into `Known(false)`.
///
/// Lock-free like [`run_liveness`]: [`crate::state::RunState::save`] writes by
/// atomic rename, so this always observes a complete file.
fn run_state_open(cfg: &Config, cwd: &Path, run_id: &str) -> Determination<bool> {
    let path = match crate::state::run_state_path(cfg, cwd, run_id) {
        Ok(p) => p,
        Err(e) => {
            return Determination::undetermined(format!(
                "run-state path for {run_id} unresolvable: {e}"
            ))
        }
    };
    let txt = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Determination::Known(false),
        Err(e) => {
            return Determination::undetermined(format!(
                "run state {} unreadable: {e}",
                path.display()
            ))
        }
    };
    match serde_json::from_str::<crate::state::RunState>(&txt) {
        Ok(rs) => {
            let (done, total) = rs.counts();
            Determination::Known(done < total)
        }
        Err(e) => Determination::undetermined(format!("run state {} corrupt: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::super::claim_files;
    use super::super::tests::{files, make_cfg, make_tmp_dir, registry_path};
    use super::*;

    /// A fresh claim ⇒ `Live`. Establishes liveness via the real claim path.
    #[test]
    fn run_liveness_live_for_fresh_claim() {
        let tmp_dir = make_tmp_dir("live");
        let tmp = tmp_dir.path();
        let cfg = make_cfg(tmp);
        let now = 1_000_000;
        claim_files(&cfg, tmp, "run", Some("sess"), &files(&["a.txt"]), now).unwrap();
        assert_eq!(run_liveness(&cfg, tmp, "run", now), RunLiveness::Live);
    }

    /// A genuinely-absent registry AND no run-state file for the run ⇒ `Dead`
    /// (no claims exist for anyone, and nothing says the run is still open).
    /// It must stay `Dead` so a stale hold from a gone run does not block
    /// forever. (With an OPEN run state the same empty registry is `Live` —
    /// backlog 4fa94cba, see `run_liveness_live_when_no_claims_but_run_state_open`.)
    #[test]
    fn run_liveness_dead_when_registry_absent() {
        let tmp_dir = make_tmp_dir("absent");
        let tmp = tmp_dir.path();
        let cfg = make_cfg(tmp);
        // No claim ever written → claims.json does not exist.
        assert!(!registry_path(&cfg, tmp).exists());
        assert_eq!(run_liveness(&cfg, tmp, "run", 1_000_000), RunLiveness::Dead);
    }

    /// A claim whose heartbeat is past the stuck-TTL ⇒ `Dead` (the placer went
    /// silent — the reaper's own staleness).
    #[test]
    fn run_liveness_dead_when_claim_stale() {
        let tmp_dir = make_tmp_dir("stale");
        let tmp = tmp_dir.path();
        let cfg = make_cfg(tmp);
        let claimed_at = 1_000_000;
        claim_files(
            &cfg,
            tmp,
            "run",
            Some("sess"),
            &files(&["a.txt"]),
            claimed_at,
        )
        .unwrap();
        let now = claimed_at + cfg.stuck_ttl_secs as i64 + 100;
        assert_eq!(run_liveness(&cfg, tmp, "run", now), RunLiveness::Dead);
    }

    /// A present-but-UNREADABLE registry ⇒ `Undetermined`, NOT `Dead`. This is
    /// the fail-closed pin: a corrupt claims.json must never be silently read as
    /// "the placer is dead", which would let the merge-hold gate drop a hold it
    /// cannot vouch for. RED against the old `load()`-to-empty behavior (which
    /// returned "not live" ⇒ the gate would map it to Dead and merge).
    #[test]
    fn run_liveness_undetermined_when_registry_unreadable() {
        let tmp_dir = make_tmp_dir("corrupt");
        let tmp = tmp_dir.path();
        let cfg = make_cfg(tmp);
        let path = registry_path(&cfg, tmp);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{ this is not valid json ]]").unwrap();
        assert_eq!(
            run_liveness(&cfg, tmp, "run", 1_000_000),
            RunLiveness::Undetermined
        );
    }

    /// An empty `run_id` (an unattributed hold, no placer to test) ⇒
    /// `Undetermined`, so the gate keeps blocking rather than dropping a hold on
    /// ambiguous data.
    #[test]
    fn run_liveness_undetermined_for_empty_run_id() {
        let tmp_dir = make_tmp_dir("empty-id");
        let tmp = tmp_dir.path();
        let cfg = make_cfg(tmp);
        assert_eq!(
            run_liveness(&cfg, tmp, "", 1_000_000),
            RunLiveness::Undetermined
        );
    }

    /// Save a run state for `run_id` whose tasks carry `statuses`.
    fn save_run_state(cfg: &Config, cwd: &Path, run_id: &str, statuses: &[crate::state::Status]) {
        let rs = crate::state::RunState {
            run_id: run_id.to_string(),
            goal: "g".into(),
            tasks: statuses
                .iter()
                .enumerate()
                .map(|(i, st)| crate::state::TaskState {
                    id: format!("t{i}"),
                    status: *st,
                    ..Default::default()
                })
                .collect(),
            paused: false,
            terminal_label: None,
            recorded_at: None,
            recorded_episodes: Vec::new(),
        };
        rs.save(cfg, cwd).unwrap();
    }

    /// backlog 4fa94cba: no claims anywhere (registry absent) but the run state
    /// has a non-terminal task ⇒ `Live`, not `Dead`.
    #[test]
    fn run_liveness_live_when_no_claims_but_run_state_open() {
        let tmp_dir = make_tmp_dir("open-state");
        let tmp = tmp_dir.path();
        let cfg = make_cfg(tmp);
        use crate::state::Status;
        save_run_state(&cfg, tmp, "run", &[Status::Verified, Status::Pending]);
        assert!(!registry_path(&cfg, tmp).exists());
        assert_eq!(run_liveness(&cfg, tmp, "run", 1_000_000), RunLiveness::Live);
    }

    /// A stale claim does not make an open run dead either: the run state is
    /// still consulted after the claim ages out.
    #[test]
    fn run_liveness_live_when_claim_stale_but_run_state_open() {
        let tmp_dir = make_tmp_dir("stale-open");
        let tmp = tmp_dir.path();
        let cfg = make_cfg(tmp);
        let claimed_at = 1_000_000;
        claim_files(
            &cfg,
            tmp,
            "run",
            Some("sess"),
            &files(&["a.txt"]),
            claimed_at,
        )
        .unwrap();
        save_run_state(&cfg, tmp, "run", &[crate::state::Status::Done]);
        let now = claimed_at + cfg.stuck_ttl_secs as i64 + 100;
        assert_eq!(run_liveness(&cfg, tmp, "run", now), RunLiveness::Live);
    }

    /// No claims and every task terminal ⇒ `Dead` (a finished run's stale hold
    /// must still age out).
    #[test]
    fn run_liveness_dead_when_no_claims_and_run_state_terminal() {
        let tmp_dir = make_tmp_dir("terminal-state");
        let tmp = tmp_dir.path();
        let cfg = make_cfg(tmp);
        use crate::state::Status;
        save_run_state(
            &cfg,
            tmp,
            "run",
            &[Status::Verified, Status::Cancelled, Status::Discarded],
        );
        assert_eq!(run_liveness(&cfg, tmp, "run", 1_000_000), RunLiveness::Dead);
    }

    /// No claims and a run-state file that exists but is corrupt ⇒
    /// `Undetermined` (fail closed), never `Dead`.
    #[test]
    fn run_liveness_undetermined_when_no_claims_and_run_state_corrupt() {
        let tmp_dir = make_tmp_dir("corrupt-state");
        let tmp = tmp_dir.path();
        let cfg = make_cfg(tmp);
        let path = crate::state::run_state_path(&cfg, tmp, "run").unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{ not json ]]").unwrap();
        assert_eq!(
            run_liveness(&cfg, tmp, "run", 1_000_000),
            RunLiveness::Undetermined
        );
    }
}
