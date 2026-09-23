//! Cross-session file-claim registry — stops two condukt sessions on the SAME
//! machine from processing overlapping work (PDO = Parallel Development
//! Orchestration).
//!
//! # The gap this closes
//!
//! `state::cross_run_conflicts` (Phase 3.5) is an *advisory one-time snapshot*:
//! it compares an incoming decomposition's `touched_files` against other open
//! runs at schedule time. Two sessions racing at the same instant — both before
//! either has written its run-state — never see each other, and it never
//! *enforces* a skip. The per-run [`crate::lock::RunLock`] only serializes writes
//! to the *same* run; each session has its own run, so cross-run work is not
//! serialized at all.
//!
//! This registry is the missing piece: a project-scoped JSON file that every
//! session on the machine keeps writing its live claims into, and reads before
//! executing a task. It is keyed by the file paths/globs a task touches — the one
//! identity that is meaningful *across* runs (task ids like `t1` are per-run and
//! not comparable). Overlap is decided with the same glob-aware
//! [`crate::schedule::files_conflict`] the scheduler already uses.
//!
//! # Behavior (design decisions, locked with the user)
//!
//! - **File-level lease**: a session claims the `touched_files` of the task it is
//!   about to run.
//! - **Hard skip**: if any of those files overlap with a claim held by a *live*
//!   holder from *another* run, the task is not run ("already being processed →
//!   don't process it"). Non-conflicting files are still claimed so the rest of a
//!   batch proceeds (partial progress).
//! - **Heartbeat**: a session refreshes `heartbeat_at` while it works (the "keep
//!   writing to a file" part), so a live-but-quiet holder is not reaped.
//! - **Stale reap**: a claim whose heartbeat is older than the stuck-TTL is
//!   reaped so a dead session never blocks others forever. Liveness is anchored
//!   to the heartbeat, NOT the recorded pid — the condukt CLI is ephemeral, so a
//!   pid check would read every claim as dead on the next invocation.
//!
//! # Where the registry lives: the MAIN worktree root, not `repo_root`
//!
//! The store is keyed by [`harness_core::projkey::main_worktree_root`], so every
//! linked worktree of a repository shares ONE `claims.json`. Run state is
//! deliberately NOT keyed that way (it still uses `repo_root`).
//!
//! The contract at the top of this module is what forces the difference: this
//! registry exists to stop "two condukt sessions on the SAME machine from
//! processing overlapping work". `projkey::repo_root` stops at the first
//! ancestor holding a `.git` ENTRY, and in a linked worktree `.git` is a file —
//! so it returns the worktree itself. Keying on it gave each worktree its own
//! private registry, and under CLAUDE.md §8 (all work happens in a linked
//! worktree) that is precisely where the guard had to hold: a claim taken in one
//! worktree read as "not claimed" from every other, so the last guard against
//! two sessions taking the same task answered "free" (backlog `d83b0e8f`).
//!
//! Two consequences, stated plainly:
//!
//! * **Migration**: claims written under the old worktree-derived key are not
//!   moved. They are orphaned, and age out via the heartbeat TTL like any other
//!   unreleased claim.
//! * **Scope**: run state, precedent and autoflow's stores still key off
//!   `repo_root` and are untouched here — relocating them would orphan live
//!   on-disk state. That wider migration is backlog `43393ce2`.
//!
//! # Concurrency & safety
//!
//! Every mutation is a load → mutate → save cycle guarded by the proven
//! [`crate::lock::RunLock`], keyed on the reserved id [`CLAIMS_LOCK_KEY`] so all
//! sessions serialize on one lock file (`<project>/__claims__.lock`) and no update
//! is lost. Writes are atomic (temp + rename). A MISSING registry is fail-soft
//! (it is a legitimately empty one — nobody has claimed yet); a present-but
//! -UNPARSEABLE one is NOT: it is "cannot determine" ([`load`]), and every
//! consumer refuses rather than act on a phantom empty set or save over claims
//! it could not read. The module never panics.

use crate::config::Config;
use crate::lock::RunLock;
use crate::schedule::files_conflict;
use crate::store::project_key;
// `repo_root` is only referenced by this module's fixtures, which assert what the
// OLD (per-worktree) key resolved to; production code keys off
// `claim_project_root` instead.
#[cfg(test)]
use crate::store::repo_root;
use anyhow::Result;
use harness_core::progress::{self, Liveness};
use harness_core::verdict::{Determination, Required};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Reserved run-id used only to key the registry's own RMW lock file
/// (`<project>/__claims__.lock`), reusing the proven per-run lock. It never names
/// a real run, so it cannot collide with one.
pub(crate) const CLAIMS_LOCK_KEY: &str = "__claims__";

/// Synthetic holder run-id stamped on a [`Skipped`] entry when the claims-registry
/// lock was contended past its deadline (rather than a real peer run holding the
/// file). Marks a fail-CLOSED skip so the caller HARD-SKIPS its task instead of
/// proceeding to an unlocked read-modify-write that could double-claim.
const LOCK_CONTENDED_HOLDER: &str = "__lock_contended__";

/// Synthetic holder run-id stamped on a [`Skipped`] entry when the registry's
/// PROJECT ROOT could not be resolved ([`claim_project_root`] returned
/// `Undetermined`), so we do not know which `claims.json` this repo's sessions
/// share and cannot tell whether the work is already claimed. Fail-CLOSED: the
/// caller hard-skips.
///
/// It is deliberately NOT [`LOCK_CONTENDED_HOLDER`]. Those are different facts —
/// "a peer is mid-write" versus "I cannot locate the registry at all" — and
/// collapsing them into one marker would make the second invisible in the skip
/// JSON, which is where an operator reads why a task did not run.
const PROJECT_UNDETERMINED_HOLDER: &str = "__project_undetermined__";

/// Fail-CLOSED outcome: every requested identity (file path or task hashkey) is
/// reported skipped and none claimed, so the caller treats them as unavailable
/// and skips the task. `holder` records WHICH refusal this was — see
/// [`LOCK_CONTENDED_HOLDER`] and [`PROJECT_UNDETERMINED_HOLDER`].
fn all_skipped_by(idents: &[String], holder: &str) -> ClaimOutcome {
    ClaimOutcome {
        claimed: Vec::new(),
        skipped: idents
            .iter()
            .map(|f| Skipped {
                file: f.clone(),
                holder_run: holder.to_string(),
                holder_pid: 0,
                holder_session: None,
            })
            .collect(),
    }
}

/// Fail-CLOSED outcome for a contended claims-registry lock. This is the whole
/// point of the hard-skip lock — under pathological contention two timed-out
/// writers must NOT both proceed to an unlocked RMW and double-claim the same
/// work (last-writer-wins). The synthetic [`LOCK_CONTENDED_HOLDER`] distinguishes
/// a contention skip from a real peer-run skip.
fn all_skipped(idents: &[String]) -> ClaimOutcome {
    all_skipped_by(idents, LOCK_CONTENDED_HOLDER)
}

/// Fail-CLOSED outcome for an unresolvable project root, marked with
/// [`PROJECT_UNDETERMINED_HOLDER`]. `why` is echoed to stderr so the refusal is
/// never silent (CLAUDE.md §4): the JSON says WHICH refusal it was, stderr says
/// why the resolution failed.
fn all_skipped_project_undetermined(idents: &[String], why: &str) -> ClaimOutcome {
    eprintln!(
        "condukt: claim registry project root undetermined ({why}); hard-skipping \
         {} identity(ies) rather than claiming against an unknown registry",
        idents.len()
    );
    all_skipped_by(idents, PROJECT_UNDETERMINED_HOLDER)
}

/// Test-only race-window widener for [`claim_tasks`]'s load->check->save section.
/// Real process-spawn overhead dwarfs that section's natural duration, so two
/// racing `condukt state claim-task` processes essentially never interleave in
/// it by chance — a concurrency regression test needs a deterministic way to
/// force the interleave inside the [`RunLock`]-held critical section. No-op
/// unless `CONDUKT_TEST_CLAIM_DELAY_MS` is set, so production behavior is
/// unchanged. As of condukt 0.7.162 exactly two integration tests set it:
/// `run_lock_concurrency` (150ms) and `claim_worktree_scope`'s
/// `p5_claim_from_main_tree_contends_on_the_same_claims_lock_as_the_linked_worktree`
/// (14000ms). Grep before asserting the list is still complete — the previous
/// wording claimed the var was "never set outside the `run_lock_concurrency`
/// integration test" and was already false when the second setter landed.
/// Mirrors `overwatch::lease::artificial_race_delay`.
fn artificial_race_delay() {
    if let Some(ms) = std::env::var("CONDUKT_TEST_CLAIM_DELAY_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
}

/// One live occupancy of a file path/glob by a session's run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claim {
    /// The condukt run that holds this file.
    pub run_id: String,
    /// The Claude session that owns the run (`CLAUDE_CODE_SESSION_ID`), if known.
    #[serde(default)]
    pub session_id: Option<String>,
    /// OS pid of the process that last (re)claimed/heartbeat this file — used for
    /// liveness checks and stale reaping.
    pub pid: u32,
    /// When the file was first claimed by this run (unix seconds).
    pub claimed_at: i64,
    /// Last liveness refresh (unix seconds). Bumped on every (re)claim/heartbeat.
    pub heartbeat_at: i64,
    /// Human-readable task title, for inspection / the execution-state view. Only
    /// task-level claims carry it; file-level claims leave it `None`. Kept optional
    /// (and skipped when absent) so existing file-claim JSON stays byte-identical
    /// and old registries without the field still load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// A file that could not be claimed because a live holder from another run owns
/// an overlapping path/glob.
#[derive(Debug, Clone, Serialize)]
pub struct Skipped {
    pub file: String,
    pub holder_run: String,
    pub holder_pid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holder_session: Option<String>,
}

/// Result of a [`claim_files`] attempt: which files we now hold, and which were
/// skipped (held live by another run). A non-empty `skipped` means the caller
/// must NOT process the corresponding task (hard skip).
#[derive(Debug, Default, Serialize)]
pub struct ClaimOutcome {
    pub claimed: Vec<String>,
    pub skipped: Vec<Skipped>,
}

/// The on-disk registry: two parallel occupancy tables sharing one lock and one
/// RMW cycle.
///
/// - `files`: file path/glob -> its live holder (the original PDO collision guard).
///   Flattened to the top level so a pre-existing `claims.json` written as a bare
///   `{ "<path>": {..} }` map still deserializes here unchanged (backward compat).
/// - `task_claims`: task hashkey -> its live holder. Hashkeys are opaque strings
///   supplied by callers (computed elsewhere); this module never derives them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    /// File path/glob -> live holder. Flattened for backward-compatible on-disk
    /// layout with the original bare-map format.
    #[serde(flatten)]
    pub files: BTreeMap<String, Claim>,
    /// Task hashkey -> live holder. Absent in old registries, so defaulted.
    #[serde(default)]
    pub task_claims: BTreeMap<String, Claim>,
}

/// The root this registry is keyed by: the MAIN worktree root of the repo
/// containing `cwd`, so all of that repo's linked worktrees address ONE store.
///
/// Why claims use this while run state does not: the module contract above says
/// this registry "stops two condukt sessions on the SAME machine from processing
/// overlapping work". "The same machine" is a REPO-wide scope, not a
/// per-checkout one, and under CLAUDE.md §8 the sessions it must separate are
/// each in their own linked worktree — so a per-worktree key makes the guard
/// answer "free" exactly when two sessions collide. Run state is the opposite
/// case: it is per-run bookkeeping that already lives on disk under
/// `repo_root`-derived keys, and re-keying it would orphan those files (backlog
/// `43393ce2`).
///
/// `Undetermined` is a legitimate answer and is never collapsed into a path: see
/// [`harness_core::projkey::main_worktree_root`] for the arms.
fn claim_project_root(cwd: &Path) -> Determination<PathBuf> {
    harness_core::projkey::main_worktree_root(cwd)
}

/// `<state_dir>/<project-key>` for an already-resolved project root.
fn project_dir(cfg: &Config, project_root: &Path) -> PathBuf {
    cfg.state_dir.join(project_key(project_root))
}

/// The pair every registry read-modify-write needs, resolved ONCE: the project
/// root (which keys both `claims.json` AND its `__claims__` lock) and the
/// registry path. Resolving them together is what keeps the file and its lock
/// from being derived by two different routes.
fn claim_paths(cfg: &Config, cwd: &Path) -> Determination<(PathBuf, PathBuf)> {
    claim_project_root(cwd).map(|root| {
        let path = project_dir(cfg, &root).join("claims.json");
        (root, path)
    })
}

/// `<state_dir>/<project-key>/claims.json` — beside the run-state files, so it is
/// per-project and unrelated projects never share a registry. `Undetermined`
/// when the project root cannot be resolved; callers must fail closed.
fn registry_path(cfg: &Config, cwd: &Path) -> Determination<PathBuf> {
    claim_paths(cfg, cwd).map(|(_, path)| path)
}

/// `<state_dir>/<project-key>/execution-state.json` — the joined "who is running
/// what" view, written beside `claims.json`.
// Wired into the CLI by the follow-up task; used by tests today.
#[allow(dead_code)]
fn execution_state_path(cfg: &Config, cwd: &Path) -> Determination<PathBuf> {
    claim_project_root(cwd).map(|root| project_dir(cfg, &root).join("execution-state.json"))
}

/// Read the registry as a three-valued observation: an ABSENT file is a
/// legitimately empty registry ([`Determination::Known`] — nobody has claimed
/// yet, which is the normal state of a fresh project), while a present-but
/// -UNPARSEABLE file, or any other read failure, is
/// [`Determination::Undetermined`] — the claims that exist could not be
/// observed.
///
/// This used to collapse both into an empty [`Registry`], documented as
/// fail-soft ("proceeding is safer than aborting a state transition"). It was
/// not: every consumer reads an empty registry as "no live holder ⇒ free to
/// claim", so ONE corrupt `claims.json` made every other session's claims
/// vanish and the next claim then SAVED an empty registry over them — the mass
/// double-claim this module exists to prevent. That is the canonical fail-open
/// shape of CLAUDE.md §3: the empty set standing in for "could not check".
///
/// The value is only reachable through [`Determination::require`], so each
/// caller must spell out, in the diff, what an unreadable registry means for
/// it. Every caller here refuses (see [`load_or_refuse`]); the absent-file arm
/// is what keeps that from degrading into a blanket "always refuse".
fn load(path: &Path) -> Determination<Registry> {
    match read_registry(path) {
        Ok(Some(reg)) => Determination::known(reg),
        // Genuinely absent (ENOENT): an observation of "no claims", not a
        // failure to look.
        Ok(None) => Determination::known(Registry::default()),
        Err(e) => Determination::undetermined(format!(
            "claim registry {} exists but could not be read/parsed: {e}",
            path.display()
        )),
    }
}

/// [`load`] for the callers that must not act on a phantom empty registry:
/// yields the observed [`Registry`], or an error naming `action` as the thing
/// being refused.
///
/// Every current consumer of [`load`] resolves `Undetermined` this way, for one
/// of two reasons, both restrictive per CLAUDE.md §3:
///
/// - it would GRANT a claim ([`claim_files_with_deadline`], [`claim_tasks`])
///   against a set of holders it cannot see, i.e. double-claim;
/// - or it would SAVE the phantom-empty registry back
///   ([`release_run`], [`release_files`], [`release_tasks`], [`heartbeat`],
///   [`active_claims`], [`write_execution_state`]), destroying the claims it
///   failed to parse — turning a transient unreadable file into permanent data
///   loss for every other session.
///
/// A refusal costs nothing that is not already lost: an unreleased claim ages
/// out through the heartbeat TTL, and the caller sees WHY instead of a silent
/// empty answer.
fn load_or_refuse(path: &Path, action: &str) -> Result<Registry> {
    match load(path).require() {
        Required::Determined(reg) => Ok(reg),
        Required::Blocked(v) => {
            let why = match v.reason() {
                Some(r) => r.as_str().to_string(),
                None => "no reason recorded".to_string(),
            };
            anyhow::bail!(
                "refusing to {action}: {why}. The registry is NOT empty — its \
                 contents are unknown, so acting on it could double-claim files \
                 another live session holds, or overwrite their claims. Inspect \
                 (and, if it is genuinely garbage, remove) {} to recover.",
                path.display()
            )
        }
    }
}

/// Atomic write (temp + rename), mirroring `RunState::save`. Generic so it serves
/// both the registry and the derived execution-state view.
fn save<T: Serialize>(path: &Path, val: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(val)?;
    let tmp = unique_tmp(path, "json");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Process-wide monotonic counter so two saves in the SAME process (identical
/// pid, and possibly an identical `now_unix_nanos()` under a coarse clock) never
/// derive the same temp name. Without it a nanos collision between two concurrent
/// degraded writers (e.g. after a fail-soft lock timeout) could have both write
/// the SAME fixed `json.tmp` path, so a rename publishes a half-written registry —
/// which no longer parses, so (since [`load`] became tri-state) every claim and
/// release against it is REFUSED until a human repairs the file.
/// Mirrors `lock::TMP_SEQ` / `overwatch::store::TMP_SEQ`.
static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn now_unix_nanos() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Build a per-call-unique sibling temp path for an atomic save:
/// `<stem>.<tag>.tmp.<pid>.<nanos>.<seq>`. Two concurrent saves of the same
/// registry therefore never collide on a single temp name (which would let one
/// publish a half-written file the other renames into place), mirroring
/// `lock.rs`'s `TMP_SEQ`/`now_unix_nanos` idiom.
fn unique_tmp(path: &Path, tag: &str) -> PathBuf {
    use std::sync::atomic::Ordering;
    path.with_extension(format!(
        "{tag}.tmp.{}.{}.{}",
        std::process::id(),
        now_unix_nanos(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ))
}

/// A claim's heartbeat is stale when it is older than the TTL — the session that
/// held it went QUIET (died, or stalled past the stuck-TTL) without releasing.
///
/// Heartbeat-staleness is NECESSARY but NOT SUFFICIENT to reap: a quiet session
/// may still be doing durable work (committing, growing its transcript, advancing
/// tasks). The actual reap ([`reap`]/[`retain_claim`]) additionally requires a
/// confirmed `Known(Stalled)` PROGRESS verdict ([`claim_progress`]) before it
/// steals the claim; a stale-but-progressing (or unconfirmable) holder is kept.
///
/// Staleness is anchored to the **heartbeat**, NOT to `c.pid`: the condukt CLI is
/// ephemeral (each invocation is a separate short-lived process that exits
/// immediately), so the recorded pid is essentially always dead by the next
/// command. Reaping on pid-liveness would therefore wipe every claim on the very
/// next invocation. A live session instead proves liveness by refreshing the
/// heartbeat (via `state set`/`state heartbeat`); `c.pid` is retained only for
/// observability (which condukt process last touched the claim). The TTL reuses
/// the STUCK TTL, so a claim becomes ELIGIBLE for a progress-gated reap exactly
/// when its task would already be considered stuck.
fn is_stale(c: &Claim, now: i64, ttl: i64) -> bool {
    now.saturating_sub(c.heartbeat_at) > ttl
}

/// Directory where per-run progress snapshots are persisted (one file per run,
/// keyed by run id). Colocated with the claim registry's state dir so it is torn
/// down with the run's state, never leaking across projects.
/// `Undetermined` when the project root cannot be resolved. It is NOT defaulted
/// to a relative `progress/` directory (which the previous `unwrap_or_else` did):
/// a progress store that silently moved to the process's cwd has no prior
/// samples, and "no prior sample" is read by [`progress::sample`] as a verdict
/// this module's reap gate must not act on.
pub(crate) fn progress_store_dir(cfg: &Config, cwd: &Path) -> Determination<PathBuf> {
    claim_project_root(cwd).map(|root| project_dir(cfg, &root).join("progress"))
}

/// Multi-signal, multi-sample PROGRESS verdict for the run owning claim `c`.
///
/// A heartbeat lapsing past the stuck-TTL proves only that the session went
/// QUIET, not that it DIED — the reap must not force-steal a holder that is
/// still doing durable work. This samples three durable signals across the
/// [`progress`] window and returns: `Known(Stalled)` only when the whole
/// fingerprint is frozen for the full window; `Known(Progressing)` when any
/// signal advanced; `Undetermined` when a signal is unreadable, there is no
/// prior sample yet, or the window has not elapsed. Reap fires ONLY on
/// `Known(Stalled)` — Progressing and Undetermined both preserve the claim
/// (fail-closed).
///
/// # Every signal is RUN-scoped, never repo-wide
///
/// The three signals are:
///
/// * `run-worktree-heads` — git HEAD of each worktree **this run's own tasks**
///   record ([`run_worktree_head_signal`]).
/// * `transcript` — the owning session's transcript growth.
/// * `run-tasks` — the max `updated_at` across this run's tasks.
///
/// The head signal used to be `git_head_signal(&repo_root(cwd))`, the HEAD of
/// the **whole project repo**. Under CLAUDE.md §8 other sessions always exist
/// and always commit to that repo, so that signal moved for reasons having
/// nothing to do with this claim's holder, and it cut both ways: a DEAD holder
/// read `Known(Progressing)` forever and could never be reclaimed, while a LIVE
/// holder that had committed inside its own worktree read `Known(Stalled)` when
/// the shared repo happened to be quiet — and this gate, unlike its `state
/// probe` twin, holds reap authority, so that force-stole a demonstrably alive
/// holder's claim. A commit made elsewhere in the shared repo is neither
/// evidence that this run advanced nor evidence that it did not.
///
/// The run state is loaded ONCE here and feeds both run-scoped signals; a run
/// state that cannot be read leaves both of them `Undetermined` (the claim is
/// kept).
fn claim_progress(cfg: &Config, cwd: &Path, c: &Claim, now: i64) -> Determination<Liveness> {
    #[cfg(test)]
    if let Some(forced) = test_hook::forced_progress() {
        return forced;
    }
    // One load, both run-scoped signals — and therefore exactly one
    // load-failure arm: without the run state NEITHER of them can be read.
    let (head, task_progress) = match crate::state::RunState::load(cfg, cwd, &c.run_id) {
        Ok(rs) => (run_worktree_head_signal(&rs), run_task_progress_signal(&rs)),
        Err(e) => {
            let unreadable: Determination<Vec<u8>> =
                Determination::undetermined(format!("run state for {} unreadable: {e}", c.run_id));
            (unreadable.clone(), unreadable)
        }
    };
    let transcript = match c.session_id.as_deref() {
        Some(sid) => progress::session_transcript_signal(sid),
        None => Determination::undetermined(
            "claim has no owning session id — transcript progress unreadable",
        ),
    };
    let current = progress::fingerprint_from_signals(vec![
        ("run-worktree-heads", head),
        ("transcript", transcript),
        ("run-tasks", task_progress),
    ]);
    let key = format!("claim:{}", c.run_id);
    // Fail-CLOSED: without a resolvable store there is nowhere to compare
    // against, so the verdict is Undetermined — and `retain_claim` KEEPS a claim
    // whose progress is Undetermined, so an unresolvable project root can never
    // force-steal someone's claim.
    let store = match progress_store_dir(cfg, cwd) {
        Determination::Known(d) => d,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    progress::sample(
        &store,
        &key,
        current,
        now,
        progress::window_secs(progress::DEFAULT_WINDOW_SECS),
    )
}

/// The value [`run_worktree_head_signal`] folds in for a recorded worktree that
/// `stat()` reports as absent. It is deliberately NOT a valid git object id
/// (those are lowercase hex), so it can never alias a real HEAD, and it is a
/// constant, so an absent worktree contributes the SAME bytes at every sample —
/// an absent worktree must read as frozen, not as churn.
const ABSENT_WORKTREE_MARKER: &[u8] = b"<absent-worktree>";

/// RUN-scoped durable head signal: the git HEAD of every worktree that `run`'s
/// own tasks record, folded into one deterministic value.
///
/// This is the run-scoped counterpart of `state::probe_run`'s task-scoped
/// `task-worktree-head`: a claim is held per RUN, and `c.run_id` covers ALL of
/// that run's tasks, so the holder is progressing if ANY of its worktrees
/// advanced. The HEADs are taken in task-id order and each is folded in
/// **alongside its task id**, so the value is stable across run-state
/// reorderings and two tasks that swap HEADs cannot alias to the same value.
///
/// Two "cannot determine" arms, both protective (CLAUDE.md §3):
///
/// * **No task records a worktree** (serial / fast-path / single-worktree mode) ⇒
///   `Undetermined`. There is no run-scoped durable head signal to read at all.
///   It deliberately does NOT fall back to the repo-wide HEAD — that fallback is
///   the defect described on [`claim_progress`], and it resolves "I cannot see
///   this run" to a signal that another session controls.
/// * **A recorded worktree EXISTS but git cannot be asked about it** (malformed
///   or absent `.git`, permissions, transient IO) ⇒ `Undetermined` for the whole
///   signal, not "that one contributed nothing". A failed read must never be
///   summed into a value that can then read as frozen and fire a reap.
///
/// An **ABSENT** worktree is the one case that is deliberately NOT
/// `Undetermined`. `stat()` answering `NotFound` is a SUCCESSFUL observation:
/// git can never advance a HEAD in a directory that does not exist, so that
/// worktree will never contribute progress again. It folds in as the explicit,
/// stable [`ABSENT_WORKTREE_MARKER`] alongside its task id — the same shape as a
/// real head entry, so the fingerprint stays comparable across samples. It is a
/// marker rather than an omission on purpose: omitting absent worktrees would
/// silently degrade a mixed run to its surviving worktrees, and degrade an
/// all-absent run to a constant empty fold that reads as frozen for the wrong
/// reason. Conflating absence with unreadability is what made a deleted run's
/// claims immortal (backlog 5e62b0eb: 10 file claims held 40.4 days against an
/// 1800s TTL).
///
/// The two cases are NOT distinguishable from `git_head_signal`'s error — `git
/// rev-parse HEAD` exits 128 for an absent directory AND for a malformed `.git`
/// alike — so existence is checked as its own observation, before git is asked.
fn run_worktree_head_signal(run: &crate::state::RunState) -> Determination<Vec<u8>> {
    let mut worktrees: Vec<(&str, &str)> = run
        .tasks
        .iter()
        .filter_map(|t| t.worktree.as_deref().map(|wt| (t.id.as_str(), wt)))
        .collect();
    // Deterministic order: the run state's task order is not a stable input.
    worktrees.sort_by(|a, b| a.0.cmp(b.0));
    if worktrees.is_empty() {
        return Determination::undetermined(format!(
            "run {} records no task worktree — no run-scoped head signal exists for it",
            run.run_id
        ));
    }
    let mut heads: Vec<(&str, Vec<u8>)> = Vec::with_capacity(worktrees.len());
    for (task_id, wt) in worktrees {
        let path = Path::new(wt);
        match std::fs::metadata(path) {
            // Determinately ABSENT: a successful observation, folded in as an
            // explicit stable marker (never omitted — see the doc comment).
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                heads.push((task_id, ABSENT_WORKTREE_MARKER.to_vec()));
                continue;
            }
            // Present but un-stat-able (permissions, transient IO): we could not
            // even establish existence, so this is a failed read, not absence.
            Err(e) => {
                return Determination::undetermined(format!(
                    "run worktree {wt} for task {task_id} could not be stat'd: {e}"
                ));
            }
            Ok(_) => {}
        }
        match progress::git_head_signal(path) {
            Determination::Known(sha) => heads.push((task_id, sha)),
            // Fail-closed: the directory is THERE but git refused, so one
            // unreadable worktree HEAD makes the run-scoped signal unreadable,
            // never a partial "rest of them are frozen".
            Determination::Undetermined(why) => return Determination::Undetermined(why),
        }
    }
    // Length-prefixed by construction, so `(task, sha)` pairs cannot be
    // re-parsed into a different pairing by a delimiter in an id.
    Determination::Known(
        progress::ProgressFingerprint::from_entries(&heads)
            .as_str()
            .as_bytes()
            .to_vec(),
    )
}

/// Durable run-progress signal: the maximum `updated_at` across `run`'s tasks.
///
/// This is deliberately NOT the run-state file mtime — heartbeats rewrite that
/// file without changing any task, so file mtime tracks LIVENESS, not progress.
/// `TaskState.updated_at` only advances on a real status/field transition, so
/// its max is a true "durable work advanced" signal. The run state is loaded by
/// the caller ([`claim_progress`]), which maps an unloadable run state to
/// `Undetermined` for this signal AND for [`run_worktree_head_signal`]
/// (protective — an unreadable signal must never read as frozen).
fn run_task_progress_signal(run: &crate::state::RunState) -> Determination<Vec<u8>> {
    let max_updated = run
        .tasks
        .iter()
        .filter_map(|t| t.updated_at)
        .max()
        .unwrap_or(0);
    Determination::Known(max_updated.to_string().into_bytes())
}

/// Decide whether a claim survives a reap. A claim with a FRESH heartbeat is
/// always kept. A claim whose heartbeat has lapsed past the TTL is kept UNLESS
/// its run's progress is confirmed `Known(Stalled)` — Progressing or
/// Undetermined preserve it (fail-closed: "cannot determine" never reaps).
fn retain_claim(
    c: &Claim,
    now: i64,
    ttl: i64,
    progress: &dyn Fn(&Claim) -> Determination<Liveness>,
) -> bool {
    if !is_stale(c, now, ttl) {
        return true;
    }
    !matches!(progress(c), Determination::Known(Liveness::Stalled))
}

/// Reap stale claims, gated on confirmed run non-progress. `progress` yields the
/// owning run's multi-sample progress verdict for a claim; a heartbeat-stale
/// claim is reaped ONLY when that verdict is `Known(Stalled)`. See
/// [`retain_claim`] and [`claim_progress`].
fn reap(
    reg: &mut Registry,
    now: i64,
    ttl: i64,
    progress: &dyn Fn(&Claim) -> Determination<Liveness>,
) {
    reg.files.retain(|_, c| retain_claim(c, now, ttl, progress));
    reg.task_claims
        .retain(|_, c| retain_claim(c, now, ttl, progress));
}

#[cfg(test)]
mod test_hook {
    use super::*;
    use std::cell::RefCell;
    thread_local! {
        static FORCED: RefCell<Option<Determination<Liveness>>> = const { RefCell::new(None) };
    }
    pub(super) fn forced_progress() -> Option<Determination<Liveness>> {
        FORCED.with(|c| c.borrow().clone())
    }
    /// Force [`claim_progress`] to return `v` for the duration of `body` (this
    /// thread only). Compiled out of production — the reap gate is exercised in
    /// tests without spawning git or racing wall-clock windows.
    pub(super) fn with_forced<R>(v: Determination<Liveness>, body: impl FnOnce() -> R) -> R {
        struct Guard;
        impl Drop for Guard {
            fn drop(&mut self) {
                FORCED.with(|c| *c.borrow_mut() = None);
            }
        }
        FORCED.with(|c| *c.borrow_mut() = Some(v));
        let _g = Guard;
        body()
    }
}

fn ttl_secs(cfg: &Config) -> i64 {
    // Reuse the STUCK TTL: a task silent past it is already considered stuck, so
    // its claim should likewise be reclaimable. Clamp to i64 defensively.
    cfg.stuck_ttl_secs.min(i64::MAX as u64) as i64
}

/// Claim `files` for `run_id`, hard-skipping any that overlap with a live holder
/// from another run. Reaps stale claims first. Returns the split of claimed vs
/// skipped files. The RMW is serialized by the reserved-key run lock.
pub fn claim_files(
    cfg: &Config,
    cwd: &Path,
    run_id: &str,
    session_id: Option<&str>,
    files: &[String],
    now: i64,
) -> Result<ClaimOutcome> {
    claim_files_with_deadline(cfg, cwd, run_id, session_id, files, now, RunLock::DEADLINE)
}

/// Deadline-parameterized core of [`claim_files`] (production passes the 10s
/// default; the wedged-holder regression test passes a short deadline to drive
/// the same skip-on-contention path fast). HARD-SKIP semantics: if the registry
/// lock cannot be acquired within `deadline` we return an [`all_skipped`]
/// outcome and NEVER touch the registry — the old fail-soft `acquire` instead
/// handed back an UNLOCKED guard and let the RMW proceed, which is exactly the
/// double-claim window this closes.
fn claim_files_with_deadline(
    cfg: &Config,
    cwd: &Path,
    run_id: &str,
    session_id: Option<&str>,
    files: &[String],
    now: i64,
    deadline: std::time::Duration,
) -> Result<ClaimOutcome> {
    let ttl = ttl_secs(cfg);
    // Fail-CLOSED: an unresolvable project root means we cannot tell which
    // registry this repo's sessions share, so every requested file is reported
    // skipped and the caller hard-skips the task.
    let (root, path) = match claim_paths(cfg, cwd) {
        Determination::Known(p) => p,
        Determination::Undetermined(why) => {
            return Ok(all_skipped_project_undetermined(files, why.as_str()))
        }
    };
    let _lock = match RunLock::acquire_or_skip_in_project_with_deadline(
        cfg,
        &root,
        CLAIMS_LOCK_KEY,
        deadline,
    ) {
        Some(l) => l,
        None => return Ok(all_skipped(files)),
    };
    // GRANT path. An unreadable registry hides exactly the holders this check
    // exists to find, so "no conflict found" would be meaningless. Refuse.
    let mut reg = load_or_refuse(&path, "claim files")?;
    reap(&mut reg, now, ttl, &|c| claim_progress(cfg, cwd, c, now));

    let mut outcome = ClaimOutcome::default();
    for f in files {
        // Does any *other* run hold an overlapping claim? Clone the holder out so
        // the immutable borrow ends before we mutate `reg` below.
        let conflict = reg
            .files
            .iter()
            .find(|(k, c)| {
                c.run_id != run_id
                    && files_conflict(std::slice::from_ref(f), std::slice::from_ref(*k))
            })
            .map(|(_, c)| c.clone());

        if let Some(holder) = conflict {
            outcome.skipped.push(Skipped {
                file: f.clone(),
                holder_run: holder.run_id,
                holder_pid: holder.pid,
                holder_session: holder.session_id,
            });
            continue;
        }

        // Free, or already ours: (re)claim and refresh the heartbeat. Preserve the
        // original claimed_at if we already held this file.
        let claimed_at = reg.files.get(f).map(|c| c.claimed_at).unwrap_or(now);
        reg.files.insert(
            f.clone(),
            Claim {
                run_id: run_id.to_string(),
                session_id: session_id.map(str::to_string),
                pid: std::process::id(),
                claimed_at,
                heartbeat_at: now,
                title: None,
            },
        );
        outcome.claimed.push(f.clone());
    }
    save(&path, &reg)?;
    Ok(outcome)
}

/// Claim `hashkeys` (opaque per-task identities computed by the caller) for
/// `run_id`, hard-skipping any already held live by a *different* run. Symmetric to
/// [`claim_files`] but keyed by exact hashkey rather than glob-aware file overlap,
/// and running in the SAME reserved-key lock / RMW cycle. Reaps stale claims first.
/// `title` is stored for the execution-state view; when `None` on a re-claim the
/// previously recorded title is preserved. Returns the split of claimed vs skipped
/// hashkeys (in [`ClaimOutcome`], where `Skipped::file` carries the hashkey).
// Wired into the CLI by the follow-up task.
#[allow(dead_code)]
pub fn claim_tasks(
    cfg: &Config,
    cwd: &Path,
    hashkeys: &[String],
    run_id: &str,
    session_id: Option<&str>,
    now: i64,
    title: Option<&str>,
) -> Result<ClaimOutcome> {
    let ttl = ttl_secs(cfg);
    // Fail-CLOSED: an unresolvable project root means we cannot tell which
    // registry this repo's sessions share, so every requested hashkey is
    // reported skipped and the caller hard-skips the task.
    let (root, path) = match claim_paths(cfg, cwd) {
        Determination::Known(p) => p,
        Determination::Undetermined(why) => {
            return Ok(all_skipped_project_undetermined(hashkeys, why.as_str()))
        }
    };
    // HARD-SKIP on contention: proceeding to an unlocked RMW here is what lets
    // two timed-out writers both claim the same hashkey (double-claim). Treat
    // every requested hashkey as unavailable so the caller skips the task.
    let _lock = match RunLock::acquire_or_skip_in_project(cfg, &root, CLAIMS_LOCK_KEY) {
        Some(l) => l,
        None => return Ok(all_skipped(hashkeys)),
    };
    // GRANT path, same reasoning as claim_files: an unseen holder is not an
    // absent one.
    let mut reg = load_or_refuse(&path, "claim tasks")?;
    reap(&mut reg, now, ttl, &|c| claim_progress(cfg, cwd, c, now));
    artificial_race_delay();

    let mut outcome = ClaimOutcome::default();
    for hk in hashkeys {
        // After reaping, any remaining holder is live. A holder from another run is
        // a hard skip; our own is a refresh.
        if let Some(holder) = reg.task_claims.get(hk) {
            if holder.run_id != run_id {
                outcome.skipped.push(Skipped {
                    file: hk.clone(),
                    holder_run: holder.run_id.clone(),
                    holder_pid: holder.pid,
                    holder_session: holder.session_id.clone(),
                });
                continue;
            }
        }

        // Free, or already ours: (re)claim and refresh. Preserve the original
        // claimed_at and any prior title when the caller passes none.
        let (claimed_at, prior_title) = reg
            .task_claims
            .get(hk)
            .map(|c| (c.claimed_at, c.title.clone()))
            .unwrap_or((now, None));
        reg.task_claims.insert(
            hk.clone(),
            Claim {
                run_id: run_id.to_string(),
                session_id: session_id.map(str::to_string),
                pid: std::process::id(),
                claimed_at,
                heartbeat_at: now,
                title: title.map(str::to_string).or(prior_title),
            },
        );
        outcome.claimed.push(hk.clone());
    }
    save(&path, &reg)?;
    Ok(outcome)
}

/// Release every claim (files AND tasks) held by `run_id` (call on run completion
/// / gate cleanup). Returns the total number of claims released.
pub fn release_run(cfg: &Config, cwd: &Path, run_id: &str) -> Result<usize> {
    let (root, path) = match claim_paths(cfg, cwd) {
        Determination::Known(p) => p,
        Determination::Undetermined(why) => {
            return Err(project_undetermined_err(
                "release a run's claims",
                cwd,
                why.as_str(),
            ))
        }
    };
    // On contention, SKIP (return 0 released) rather than run an unlocked RMW
    // that could clobber a concurrent writer's fresh claims. Safe: an unreleased
    // claim ages out via the heartbeat-TTL stale reap.
    let _lock = match RunLock::acquire_or_skip_in_project(cfg, &root, CLAIMS_LOCK_KEY) {
        Some(l) => l,
        None => return Ok(0),
    };
    // This RMW SAVES the registry back; on an unreadable one that would publish
    // an empty registry, destroying every other run's claims. Unreleased claims
    // age out via the heartbeat TTL, so refusing loses nothing durable.
    let mut reg = load_or_refuse(&path, "release this run's claims")?;
    let before = reg.files.len() + reg.task_claims.len();
    reg.files.retain(|_, c| c.run_id != run_id);
    reg.task_claims.retain(|_, c| c.run_id != run_id);
    let removed = before - reg.files.len() - reg.task_claims.len();
    save(&path, &reg)?;
    Ok(removed)
}

/// Release specific `files` held by `run_id` (call when a task reaches a terminal
/// status). Only removes files this run actually holds. Returns the count removed.
pub fn release_files(cfg: &Config, cwd: &Path, run_id: &str, files: &[String]) -> Result<usize> {
    let (root, path) = match claim_paths(cfg, cwd) {
        Determination::Known(p) => p,
        Determination::Undetermined(why) => {
            return Err(project_undetermined_err(
                "release file claims",
                cwd,
                why.as_str(),
            ))
        }
    };
    // On contention, SKIP (0 released) rather than run an unlocked RMW that
    // could clobber concurrent claims. Safe: a stale claim ages out via TTL.
    let _lock = match RunLock::acquire_or_skip_in_project(cfg, &root, CLAIMS_LOCK_KEY) {
        Some(l) => l,
        None => return Ok(0),
    };
    // Saves the registry back — see release_run: refuse rather than publish an
    // empty one over claims we could not parse.
    let mut reg = load_or_refuse(&path, "release these files")?;
    let before = reg.files.len();
    reg.files
        .retain(|k, c| !(c.run_id == run_id && files.contains(k)));
    let removed = before - reg.files.len();
    save(&path, &reg)?;
    Ok(removed)
}

/// Release specific task `hashkeys` from the task-claim table (call when a task
/// reaches a terminal status). Removes the entries regardless of which run holds
/// them — the caller owns hashkey identity. Returns the count removed.
// Wired into the CLI by the follow-up task.
#[allow(dead_code)]
pub fn release_tasks(cfg: &Config, cwd: &Path, hashkeys: &[String]) -> Result<usize> {
    let (root, path) = match claim_paths(cfg, cwd) {
        Determination::Known(p) => p,
        Determination::Undetermined(why) => {
            return Err(project_undetermined_err(
                "release task claims",
                cwd,
                why.as_str(),
            ))
        }
    };
    // On contention, SKIP (0 released) rather than run an unlocked RMW that
    // could clobber concurrent claims. Safe: a stale claim ages out via TTL.
    let _lock = match RunLock::acquire_or_skip_in_project(cfg, &root, CLAIMS_LOCK_KEY) {
        Some(l) => l,
        None => return Ok(0),
    };
    // Saves the registry back — see release_run.
    let mut reg = load_or_refuse(&path, "release these task claims")?;
    let before = reg.task_claims.len();
    reg.task_claims.retain(|k, _| !hashkeys.contains(k));
    let removed = before - reg.task_claims.len();
    save(&path, &reg)?;
    Ok(removed)
}

/// Refresh the heartbeat of every file held by `run_id` — the "keep writing to
/// the file" liveness signal that protects a live-but-quiet session from being
/// reaped. Reaps stale claims first. Returns how many claims were refreshed.
pub fn heartbeat(cfg: &Config, cwd: &Path, run_id: &str, now: i64) -> Result<usize> {
    let ttl = ttl_secs(cfg);
    let (root, path) = match claim_paths(cfg, cwd) {
        Determination::Known(p) => p,
        Determination::Undetermined(why) => {
            return Err(project_undetermined_err(
                "refresh claim heartbeats",
                cwd,
                why.as_str(),
            ))
        }
    };
    // On contention, SKIP (0 refreshed) rather than run an unlocked RMW that
    // could clobber concurrent claims. A single missed heartbeat is safe — the
    // claim only ages out after the full stuck-TTL of silence.
    let _lock = match RunLock::acquire_or_skip_in_project(cfg, &root, CLAIMS_LOCK_KEY) {
        Some(l) => l,
        None => return Ok(0),
    };
    // Saves the registry back. A missed heartbeat is survivable (the claim only
    // ages out after a full stuck-TTL of silence); publishing an empty registry
    // over unparseable bytes is not.
    let mut reg = load_or_refuse(&path, "refresh this run's heartbeat")?;
    reap(&mut reg, now, ttl, &|c| claim_progress(cfg, cwd, c, now));
    let mut n = 0;
    for c in reg.files.values_mut().chain(reg.task_claims.values_mut()) {
        if c.run_id == run_id {
            c.heartbeat_at = now;
            n += 1;
        }
    }
    save(&path, &reg)?;
    Ok(n)
}

/// Return the live registry after reaping stale entries (persisted). Used by
/// `condukt state claims` for observability.
pub fn active_claims(cfg: &Config, cwd: &Path, now: i64) -> Result<Registry> {
    let ttl = ttl_secs(cfg);
    // An unresolvable project root is an ERROR here, never an empty `Registry`:
    // `state claims` printing `{}` and `state is-claimed` exiting "not claimed"
    // is precisely how this bug read as "the work is free".
    let (root, path) = match claim_paths(cfg, cwd) {
        Determination::Known(p) => p,
        Determination::Undetermined(why) => {
            return Err(project_undetermined_err(
                "read the live claims",
                cwd,
                why.as_str(),
            ))
        }
    };
    // Observability read. On contention, return a freshly-loaded, reaped
    // snapshot WITHOUT persisting (the save is only a housekeeping compaction) —
    // proceeding to an unlocked save could clobber a concurrent writer.
    match RunLock::acquire_or_skip_in_project(cfg, &root, CLAIMS_LOCK_KEY) {
        Some(_lock) => {
            // Read-only in intent, but it persists a reap-compaction: on an
            // unreadable registry that save would wipe it. And the returned
            // value is DISPLAYED — an empty list here reads as "nobody holds
            // anything", the very confusion this change exists to remove, so
            // the undetermined state is surfaced as an error instead.
            let mut reg = load_or_refuse(&path, "list active claims")?;
            reap(&mut reg, now, ttl, &|c| claim_progress(cfg, cwd, c, now));
            save(&path, &reg)?;
            Ok(reg)
        }
        None => {
            // Contended: nothing is persisted here, but the rows are still
            // shown to (and scripted against by) the caller, so an unreadable
            // registry must not be rendered as an empty one.
            let mut reg = load_or_refuse(&path, "list active claims")?;
            reap(&mut reg, now, ttl, &|c| claim_progress(cfg, cwd, c, now));
            Ok(reg)
        }
    }
}

/// Three-valued liveness verdict for a run id in the claim registry — see
/// [`run_liveness`]. The third state ([`RunLiveness::Undetermined`]) is what
/// keeps the merge-hold gate from silently failing open: it lets the caller
/// tell "positively dead" apart from "could not find out", and fail CLOSED on
/// the latter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunLiveness {
    /// `run_id` holds ≥1 claim whose heartbeat is within the stuck-TTL.
    Live,
    /// The registry was read cleanly and holds no fresh claim for `run_id`
    /// (a genuinely-absent registry = no claims = also Dead).
    Dead,
    /// Liveness could not be established (unreadable/corrupt registry, or an
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
/// `state heartbeat` refreshes them via [`heartbeat`]), so a claim with a fresh
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
/// **Fail-closed contract.** Only a CLEAN registry read that finds no fresh
/// claim returns [`RunLiveness::Dead`]. A present-but-unreadable/corrupt
/// registry, or an empty `run_id` (an unattributed hold with no placer to test),
/// returns [`RunLiveness::Undetermined`] — never `Dead`. Collapsing "cannot read
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
    let reg = match read_registry(&path) {
        Ok(Some(reg)) => reg,
        // Genuinely-absent registry: no claims exist for anyone ⇒ Dead.
        Ok(None) => return RunLiveness::Dead,
        // Present but unreadable/corrupt: liveness UNKNOWN. Fail closed.
        Err(_) => return RunLiveness::Undetermined,
    };
    let ttl = ttl_secs(cfg);
    let live = reg
        .files
        .values()
        .chain(reg.task_claims.values())
        .any(|c| c.run_id == run_id && !is_stale(c, now, ttl));
    if live {
        RunLiveness::Live
    } else {
        RunLiveness::Dead
    }
}

/// Read the claim registry distinguishing a genuinely-absent file (`Ok(None)`)
/// from a present-but-unreadable/corrupt one (`Err`), so a caller that must fail
/// CLOSED on "cannot determine" can tell the two apart. This is the single
/// reading primitive of the module: [`load`] wraps it into the shared
/// [`Determination`] tri-state (absent ⇒ `Known(empty)`, `Err` ⇒
/// `Undetermined`) and [`run_liveness`] maps it to [`RunLiveness`] directly.
fn read_registry(path: &Path) -> std::io::Result<Option<Registry>> {
    match std::fs::read_to_string(path) {
        Ok(txt) => serde_json::from_str(&txt)
            .map(Some)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The `Err` an operation returns when the registry's project root is
/// unresolvable.
///
/// Deliberately an error, and never `Ok(0)` or an empty [`Registry`]: downstream
/// both of those read as clean facts ("nothing to release", "nothing is
/// claimed"), which is the exact fail-open backlog `d83b0e8f` is about. The
/// resolution reason travels in the message so the refusal names its cause.
fn project_undetermined_err(op: &str, cwd: &Path, why: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "condukt claim registry: cannot resolve the project (main-worktree) root for \
         {} — {why}; refusing to {op} against an unknown registry",
        cwd.display()
    )
}

/// Test-only accessor for the private registry path, so cross-module tests
/// (e.g. the worktree merge-hold gate) can seed a CORRUPT registry at exactly
/// the location [`run_liveness`] reads.
///
/// Panics when the project root is `Undetermined`. That is the sanctioned
/// test-only use of [`Required::expect`]: a fixture whose own repo layout cannot
/// be resolved is a broken fixture, not a runtime condition to degrade around.
#[cfg(test)]
pub(crate) fn registry_path_for_test(cfg: &Config, cwd: &Path) -> PathBuf {
    registry_path(cfg, cwd)
        .require()
        .expect("test fixture: the registry project root must resolve")
}

/// Who holds a task claim — the observability slice of a [`Claim`] for the
/// execution-state view.
#[allow(dead_code)] // consumed by the CLI wiring in the follow-up task
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimedBy {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub run_id: String,
    pub heartbeat_at: i64,
}

/// One row of the execution-state view: a live task claim joined against the
/// project backlog. `backlog_id`/`status`/`project` are `None` when the backlog is
/// unavailable or the claim's title matches no pending task (fail-soft join).
#[allow(dead_code)] // consumed by the CLI wiring in the follow-up task
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionEntry {
    pub hashkey: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backlog_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub claimed_by: ClaimedBy,
}

/// Coerce a backlog JSON `id` (string or number) into a display string.
#[allow(dead_code)]
fn json_id(v: Option<&serde_json::Value>) -> Option<String> {
    match v {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

/// Shell out to `backlog list --status pending --json` and return the pending
/// tasks as a list of JSON objects. Fail-soft: a missing binary, non-zero exit, or
/// unparseable output yields an empty list rather than an error (never panics).
/// Accepts either a top-level array or a `{ "tasks": [..] }` envelope.
#[allow(dead_code)]
fn backlog_pending() -> Vec<serde_json::Value> {
    let output = std::process::Command::new("backlog")
        .args(["list", "--status", "pending", "--json"])
        .output();
    let stdout = match output {
        Ok(o) if o.status.success() => o.stdout,
        _ => return Vec::new(),
    };
    match serde_json::from_slice::<serde_json::Value>(&stdout) {
        Ok(serde_json::Value::Array(a)) => a,
        Ok(serde_json::Value::Object(mut m)) => m
            .remove("tasks")
            .and_then(|t| match t {
                serde_json::Value::Array(a) => Some(a),
                _ => None,
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Pure JOIN of live task claims against backlog pending tasks, keyed on title
/// (the one field a claim and a backlog row share here — hashkeys are opaque to
/// the backlog). Split out from [`write_execution_state`] so it is testable without
/// the external `backlog` binary. Deterministic ordering (task_claims is a BTreeMap).
#[allow(dead_code)]
fn join_execution_state(
    task_claims: &BTreeMap<String, Claim>,
    pending: &[serde_json::Value],
) -> Vec<ExecutionEntry> {
    // Index pending tasks by title for the join.
    let mut by_title: BTreeMap<&str, &serde_json::Value> = BTreeMap::new();
    for t in pending {
        if let Some(title) = t.get("title").and_then(|v| v.as_str()) {
            by_title.insert(title, t);
        }
    }
    task_claims
        .iter()
        .map(|(hashkey, claim)| {
            let matched = claim
                .title
                .as_deref()
                .and_then(|t| by_title.get(t).copied());
            ExecutionEntry {
                hashkey: hashkey.clone(),
                backlog_id: matched.and_then(|t| json_id(t.get("id"))),
                title: claim.title.clone(),
                project: matched
                    .and_then(|t| t.get("project"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                status: matched
                    .and_then(|t| t.get("status"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                claimed_by: ClaimedBy {
                    session_id: claim.session_id.clone(),
                    run_id: claim.run_id.clone(),
                    heartbeat_at: claim.heartbeat_at,
                },
            }
        })
        .collect()
}

/// Aggregate the live task claims into `execution-state.json` (beside
/// `claims.json`): reap stale claims, JOIN the survivors against the backlog's
/// pending tasks, and atomically write the rows. Fail-soft on the backlog (see
/// [`backlog_pending`]) — the claims we hold are always written, with
/// `backlog_id`/`status`/`project` left absent when unjoinable. Returns the rows
/// written.
// Wired into the CLI by the follow-up task.
#[allow(dead_code)]
pub fn write_execution_state(cfg: &Config, cwd: &Path, now: i64) -> Result<Vec<ExecutionEntry>> {
    let ttl = ttl_secs(cfg);
    let (root, path) = match claim_paths(cfg, cwd) {
        Determination::Known(p) => p,
        Determination::Undetermined(why) => {
            return Err(project_undetermined_err(
                "build the execution-state view",
                cwd,
                why.as_str(),
            ))
        }
    };
    // On contention, degrade to a read-only view: compute and return the joined
    // rows from a freshly-loaded, reaped snapshot but persist NOTHING (neither
    // the registry compaction nor the execution-state file), rather than run an
    // unlocked RMW that could clobber a concurrent writer. The view regenerates
    // on the next uncontended call.
    match RunLock::acquire_or_skip_in_project(cfg, &root, CLAIMS_LOCK_KEY) {
        Some(_lock) => {
            // Persists BOTH the compacted registry and the derived view; an
            // unreadable registry would publish an empty registry and an empty
            // "who is running what" view (read as "nothing is running").
            let mut reg = load_or_refuse(&path, "write the execution-state view")?;
            reap(&mut reg, now, ttl, &|c| claim_progress(cfg, cwd, c, now));
            save(&path, &reg)?;
            let pending = backlog_pending();
            let entries = join_execution_state(&reg.task_claims, &pending);
            save(
                &project_dir(cfg, &root).join("execution-state.json"),
                &entries,
            )?;
            Ok(entries)
        }
        None => {
            // Contended: persists nothing, but still RETURNS the view, so the
            // same "empty means nothing is running" misreading applies.
            let mut reg = load_or_refuse(&path, "write the execution-state view")?;
            reap(&mut reg, now, ttl, &|c| claim_progress(cfg, cwd, c, now));
            let pending = backlog_pending();
            Ok(join_execution_state(&reg.task_claims, &pending))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    // ── path adapters ───────────────────────────────────────────────────────
    //
    // `super::registry_path` / `super::execution_state_path` return
    // `Determination<PathBuf>` since the registry moved to the MAIN worktree
    // root. Every fixture below builds its own plain temp directory, whose root
    // always resolves, so these adapters keep the existing tests calling the
    // same names with the same meaning instead of restating the resolution in
    // each one. They assert (via `expect`) rather than defaulting: a fixture
    // whose root does not resolve is broken, and must not quietly test a
    // different path than production writes.

    fn registry_path(cfg: &Config, cwd: &Path) -> PathBuf {
        super::registry_path(cfg, cwd)
            .require()
            .expect("test fixture: the registry project root must resolve")
    }

    fn execution_state_path(cfg: &Config, cwd: &Path) -> PathBuf {
        super::execution_state_path(cfg, cwd)
            .require()
            .expect("test fixture: the registry project root must resolve")
    }

    static SEQ: AtomicU64 = AtomicU64::new(0);

    /// Test-only reader that DEMANDS a determinable registry: the tests below
    /// that use it are asserting on registry contents, so an undetermined read
    /// is a test failure, never a silently-empty registry. (Production code has
    /// no such shortcut — it goes through [`load_or_refuse`].)
    fn load_known(path: &Path) -> Registry {
        match load(path).require() {
            Required::Determined(reg) => reg,
            Required::Blocked(v) => panic!(
                "registry at {} was undetermined: {:?}",
                path.display(),
                v.reason().map(|r| r.as_str().to_string())
            ),
        }
    }

    fn make_tmp_dir(tag: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let p =
            std::env::temp_dir().join(format!("condukt-claim-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    // `unique_tmp` must derive a DISTINCT temp path per call even within one
    // process (same pid, and possibly an identical coarse-clock nanos). The old
    // fixed `path.with_extension("json.tmp")` yielded ONE shared name for every
    // writer — the collision that lets two degraded writers publish a
    // half-written registry. RED with the fixed name (all equal), GREEN here.
    #[test]
    fn unique_tmp_names_are_distinct_per_call() {
        let path = make_tmp_dir("uniq").join("claims.json");
        let a = unique_tmp(&path, "json");
        let b = unique_tmp(&path, "json");
        let c = unique_tmp(&path, "json");
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
        // Still a sibling temp of the final path (same parent, distinct name).
        assert_eq!(a.parent(), path.parent());
        assert_ne!(a.file_name(), path.file_name());
    }

    // Two concurrent degraded writers saving the SAME registry to one path must
    // never leave a half-written/empty file behind: with unique temp names each
    // writer renames its OWN fully-written temp atomically, so a concurrent
    // reader always sees a complete, parseable registry. Under the old fixed
    // `json.tmp` name both writers share one temp and one can rename a partially
    // written file (corrupt → unreadable → every claim/release refused). Large
    // payload so a single `write` is not atomic at the OS level (widens the
    // corrupt window).
    #[test]
    fn concurrent_saves_never_publish_corrupt_registry() {
        let path = make_tmp_dir("concurrent-save").join("claims.json");
        // A non-trivial registry so a truncated/interleaved write fails to parse.
        let mut reg = Registry::default();
        for i in 0..400 {
            reg.files.insert(
                format!("crates/pkg/src/file_{i:04}.rs"),
                Claim {
                    run_id: format!("run-{i:04}"),
                    pid: 12345,
                    session_id: Some(format!("session-{i:04}")),
                    heartbeat_at: 1_700_000_000 + i as i64,
                    claimed_at: 1_700_000_000,
                    title: Some(format!("task number {i} with a longish title")),
                },
            );
        }

        const THREADS: usize = 8;
        const ITERS: usize = 40;
        std::thread::scope(|scope| {
            for _ in 0..THREADS {
                let path = path.clone();
                let reg = reg.clone();
                scope.spawn(move || {
                    for _ in 0..ITERS {
                        save(&path, &reg).unwrap();
                        // Interleave reads: every observed file must be a
                        // complete, parseable registry (never half-written).
                        let loaded = load_known(&path);
                        assert_eq!(
                            loaded.files.len(),
                            reg.files.len(),
                            "a concurrent reader observed a corrupt/partial registry"
                        );
                    }
                });
            }
        });

        // Final state is intact.
        let final_reg = load_known(&path);
        assert_eq!(final_reg.files.len(), reg.files.len());
    }

    /// A fresh claim ⇒ `Live`. Establishes liveness via the real claim path.
    #[test]
    fn run_liveness_live_for_fresh_claim() {
        let tmp = make_tmp_dir("live");
        let cfg = make_cfg(&tmp);
        let now = 1_000_000;
        claim_files(&cfg, &tmp, "run", Some("sess"), &files(&["a.txt"]), now).unwrap();
        assert_eq!(run_liveness(&cfg, &tmp, "run", now), RunLiveness::Live);
    }

    /// A genuinely-absent registry ⇒ `Dead` (no claims exist for anyone). This
    /// is the normal "the placer released/never wrote claims" case, and it must
    /// stay `Dead` so a stale hold from a finished run does not block forever.
    #[test]
    fn run_liveness_dead_when_registry_absent() {
        let tmp = make_tmp_dir("absent");
        let cfg = make_cfg(&tmp);
        // No claim ever written → claims.json does not exist.
        assert!(!registry_path(&cfg, &tmp).exists());
        assert_eq!(
            run_liveness(&cfg, &tmp, "run", 1_000_000),
            RunLiveness::Dead
        );
    }

    /// A claim whose heartbeat is past the stuck-TTL ⇒ `Dead` (the placer went
    /// silent — the reaper's own staleness).
    #[test]
    fn run_liveness_dead_when_claim_stale() {
        let tmp = make_tmp_dir("stale");
        let cfg = make_cfg(&tmp);
        let claimed_at = 1_000_000;
        claim_files(
            &cfg,
            &tmp,
            "run",
            Some("sess"),
            &files(&["a.txt"]),
            claimed_at,
        )
        .unwrap();
        let now = claimed_at + cfg.stuck_ttl_secs as i64 + 100;
        assert_eq!(run_liveness(&cfg, &tmp, "run", now), RunLiveness::Dead);
    }

    /// A present-but-UNREADABLE registry ⇒ `Undetermined`, NOT `Dead`. This is
    /// the fail-closed pin: a corrupt claims.json must never be silently read as
    /// "the placer is dead", which would let the merge-hold gate drop a hold it
    /// cannot vouch for. RED against the old `load()`-to-empty behavior (which
    /// returned "not live" ⇒ the gate would map it to Dead and merge).
    #[test]
    fn run_liveness_undetermined_when_registry_unreadable() {
        let tmp = make_tmp_dir("corrupt");
        let cfg = make_cfg(&tmp);
        let path = registry_path(&cfg, &tmp);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{ this is not valid json ]]").unwrap();
        assert_eq!(
            run_liveness(&cfg, &tmp, "run", 1_000_000),
            RunLiveness::Undetermined
        );
    }

    /// An empty `run_id` (an unattributed hold, no placer to test) ⇒
    /// `Undetermined`, so the gate keeps blocking rather than dropping a hold on
    /// ambiguous data.
    #[test]
    fn run_liveness_undetermined_for_empty_run_id() {
        let tmp = make_tmp_dir("empty-id");
        let cfg = make_cfg(&tmp);
        assert_eq!(
            run_liveness(&cfg, &tmp, "", 1_000_000),
            RunLiveness::Undetermined
        );
    }

    fn make_cfg(tmp: &Path) -> Config {
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

    fn files(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn first_claim_succeeds_and_persists() {
        let tmp = make_tmp_dir("first");
        let cfg = make_cfg(&tmp);
        let out = claim_files(
            &cfg,
            &tmp,
            "runA",
            Some("sessA"),
            &files(&["src/a.rs"]),
            100,
        )
        .unwrap();
        assert_eq!(out.claimed, vec!["src/a.rs".to_string()]);
        assert!(out.skipped.is_empty());
        // Persisted and visible.
        let live = active_claims(&cfg, &tmp, 100).unwrap();
        assert_eq!(live.files.len(), 1);
        assert_eq!(live.files["src/a.rs"].run_id, "runA");
    }

    #[test]
    fn conflicting_claim_from_other_run_is_hard_skipped() {
        let tmp = make_tmp_dir("conflict");
        let cfg = make_cfg(&tmp);
        claim_files(&cfg, &tmp, "runA", None, &files(&["src/a.rs"]), 100).unwrap();
        // Different run tries the same file → skipped, not claimed.
        let out = claim_files(&cfg, &tmp, "runB", None, &files(&["src/a.rs"]), 101).unwrap();
        assert!(out.claimed.is_empty());
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.skipped[0].holder_run, "runA");
        // The file is still owned by runA.
        let live = active_claims(&cfg, &tmp, 102).unwrap();
        assert_eq!(live.files["src/a.rs"].run_id, "runA");
    }

    #[test]
    fn partial_batch_claims_free_files_only() {
        let tmp = make_tmp_dir("partial");
        let cfg = make_cfg(&tmp);
        claim_files(&cfg, &tmp, "runA", None, &files(&["src/a.rs"]), 100).unwrap();
        let out = claim_files(
            &cfg,
            &tmp,
            "runB",
            None,
            &files(&["src/a.rs", "src/b.rs"]),
            101,
        )
        .unwrap();
        assert_eq!(out.claimed, vec!["src/b.rs".to_string()]);
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.skipped[0].file, "src/a.rs");
    }

    #[test]
    fn glob_overlap_is_detected() {
        let tmp = make_tmp_dir("glob");
        let cfg = make_cfg(&tmp);
        claim_files(&cfg, &tmp, "runA", None, &files(&["src/*.rs"]), 100).unwrap();
        let out = claim_files(&cfg, &tmp, "runB", None, &files(&["src/a.rs"]), 101).unwrap();
        assert!(out.claimed.is_empty(), "src/a.rs overlaps src/*.rs");
        assert_eq!(out.skipped.len(), 1);
    }

    #[test]
    fn same_run_reclaim_refreshes_heartbeat_and_keeps_claimed_at() {
        let tmp = make_tmp_dir("reclaim");
        let cfg = make_cfg(&tmp);
        claim_files(&cfg, &tmp, "runA", None, &files(&["src/a.rs"]), 100).unwrap();
        let out = claim_files(&cfg, &tmp, "runA", None, &files(&["src/a.rs"]), 200).unwrap();
        assert_eq!(out.claimed, vec!["src/a.rs".to_string()]);
        let live = active_claims(&cfg, &tmp, 200).unwrap();
        assert_eq!(live.files["src/a.rs"].claimed_at, 100);
        assert_eq!(live.files["src/a.rs"].heartbeat_at, 200);
    }

    #[test]
    fn release_files_frees_the_slot() {
        let tmp = make_tmp_dir("release");
        let cfg = make_cfg(&tmp);
        claim_files(&cfg, &tmp, "runA", None, &files(&["src/a.rs"]), 100).unwrap();
        let n = release_files(&cfg, &tmp, "runA", &files(&["src/a.rs"])).unwrap();
        assert_eq!(n, 1);
        // Now runB can take it.
        let out = claim_files(&cfg, &tmp, "runB", None, &files(&["src/a.rs"]), 101).unwrap();
        assert_eq!(out.claimed, vec!["src/a.rs".to_string()]);
    }

    #[test]
    fn release_run_frees_all_its_files() {
        let tmp = make_tmp_dir("release-run");
        let cfg = make_cfg(&tmp);
        claim_files(
            &cfg,
            &tmp,
            "runA",
            None,
            &files(&["src/a.rs", "src/b.rs"]),
            100,
        )
        .unwrap();
        let n = release_run(&cfg, &tmp, "runA").unwrap();
        assert_eq!(n, 2);
        assert!(active_claims(&cfg, &tmp, 101).unwrap().files.is_empty());
    }

    #[test]
    fn stale_claim_with_confirmed_stalled_progress_is_reaped_and_reclaimable() {
        let tmp = make_tmp_dir("stale-hb");
        let cfg = make_cfg(&tmp);
        // A claim whose heartbeat is far in the past — the holding session went
        // quiet without releasing. Heartbeat-staleness alone is NOT enough to
        // reap now; the run's progress must be CONFIRMED Stalled. Force that
        // verdict via the thread-local test seam (production samples git HEAD +
        // transcript + task updated_at across the window instead).
        let mut reg = Registry::default();
        reg.files.insert(
            "src/a.rs".to_string(),
            Claim {
                run_id: "ghost".into(),
                session_id: Some("dead-session".into()),
                pid: std::process::id(),
                claimed_at: 0,
                heartbeat_at: 0,
                title: None,
            },
        );
        save(&registry_path(&cfg, &tmp), &reg).unwrap();
        // now = ttl + 1 past the heartbeat → eligible; confirmed Stalled → reaped.
        let now = cfg.stuck_ttl_secs as i64 + 1;
        let out = test_hook::with_forced(Determination::Known(Liveness::Stalled), || {
            claim_files(&cfg, &tmp, "runB", None, &files(&["src/a.rs"]), now).unwrap()
        });
        assert_eq!(out.claimed, vec!["src/a.rs".to_string()]);
        assert!(out.skipped.is_empty());
    }

    #[test]
    fn stale_claim_but_progressing_is_not_reaped() {
        let tmp = make_tmp_dir("stale-hb-progressing");
        let cfg = make_cfg(&tmp);
        // Same heartbeat-stale claim, but the run is still doing durable work.
        // The quiet holder must be PRESERVED — reaping it would force-steal a
        // live-but-quiet session (the exact fail-open this gate closes).
        let mut reg = Registry::default();
        reg.files.insert(
            "src/a.rs".to_string(),
            Claim {
                run_id: "busy".into(),
                session_id: Some("live-quiet".into()),
                pid: std::process::id(),
                claimed_at: 0,
                heartbeat_at: 0,
                title: None,
            },
        );
        save(&registry_path(&cfg, &tmp), &reg).unwrap();
        let now = cfg.stuck_ttl_secs as i64 + 1;
        let out = test_hook::with_forced(Determination::Known(Liveness::Progressing), || {
            claim_files(&cfg, &tmp, "runB", None, &files(&["src/a.rs"]), now).unwrap()
        });
        assert!(out.claimed.is_empty(), "progressing holder must be kept");
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.skipped[0].holder_run, "busy");
    }

    #[test]
    fn stale_claim_with_undetermined_progress_is_not_reaped() {
        let tmp = make_tmp_dir("stale-hb-undet");
        let cfg = make_cfg(&tmp);
        // Progress could not be established (unreadable signals / no prior sample
        // / window not elapsed). "Cannot determine" must resolve to the
        // restrictive side: the claim is KEPT, never reaped.
        let mut reg = Registry::default();
        reg.files.insert(
            "src/a.rs".to_string(),
            Claim {
                run_id: "unknown".into(),
                session_id: Some("opaque".into()),
                pid: std::process::id(),
                claimed_at: 0,
                heartbeat_at: 0,
                title: None,
            },
        );
        save(&registry_path(&cfg, &tmp), &reg).unwrap();
        let now = cfg.stuck_ttl_secs as i64 + 1;
        let undet = Determination::undetermined("no sample yet");
        let out = test_hook::with_forced(undet, || {
            claim_files(&cfg, &tmp, "runB", None, &files(&["src/a.rs"]), now).unwrap()
        });
        assert!(
            out.claimed.is_empty(),
            "undetermined progress must NOT reap"
        );
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.skipped[0].holder_run, "unknown");
    }

    #[test]
    fn fresh_heartbeat_is_not_reaped_regardless_of_pid() {
        let tmp = make_tmp_dir("fresh-hb");
        let cfg = make_cfg(&tmp);
        // Hand-write a claim from a pid that is certainly not this process, but
        // with a fresh heartbeat: it must be RETAINED (pid is not a liveness
        // signal — the ephemeral-CLI reality).
        let mut reg = Registry::default();
        reg.files.insert(
            "src/a.rs".to_string(),
            Claim {
                run_id: "other".into(),
                session_id: Some("live-elsewhere".into()),
                pid: 424242,
                claimed_at: 100,
                heartbeat_at: 100,
                title: None,
            },
        );
        save(&registry_path(&cfg, &tmp), &reg).unwrap();
        // Within TTL of the heartbeat → still held → runB is skipped.
        let out = claim_files(&cfg, &tmp, "runB", None, &files(&["src/a.rs"]), 200).unwrap();
        assert!(out.claimed.is_empty());
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.skipped[0].holder_run, "other");
    }

    #[test]
    fn heartbeat_keeps_live_claim_protected() {
        let tmp = make_tmp_dir("hb-protect");
        let cfg = make_cfg(&tmp);
        claim_files(&cfg, &tmp, "runA", None, &files(&["src/a.rs"]), 0).unwrap();
        // Heartbeat just before the TTL window would expire.
        let ttl = cfg.stuck_ttl_secs as i64;
        heartbeat(&cfg, &tmp, "runA", ttl).unwrap();
        // A later claim from runB, within TTL of the heartbeat, is still skipped.
        let out = claim_files(&cfg, &tmp, "runB", None, &files(&["src/a.rs"]), ttl + 1).unwrap();
        assert!(out.claimed.is_empty());
        assert_eq!(out.skipped.len(), 1);
    }

    /// An UNPARSEABLE registry is cannot-determine: the claim is REFUSED and
    /// the bytes are left alone.
    ///
    /// This assertion previously read `assert_eq!(out.claimed, ["src/a.rs"])`
    /// under the name `corrupt_registry_is_treated_as_empty` — it pinned the
    /// fail-open as the contract (CLAUDE.md §2: a test can fix a defect as the
    /// spec). The behaviour it certified is the one being removed, so the test
    /// is re-pointed at the new contract rather than deleted; the claim-still
    /// -succeeds half it used to cover lives on in
    /// `absent_registry_is_empty_and_claim_succeeds` below, where absence — not
    /// corruption — is the reason the registry is empty.
    #[test]
    fn corrupt_registry_refuses_the_claim_and_is_not_clobbered() {
        let tmp = make_tmp_dir("corrupt-refuse");
        let cfg = make_cfg(&tmp);
        let path = registry_path(&cfg, &tmp);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let corrupt: &[u8] = b"not json at all {{{";
        std::fs::write(&path, corrupt).unwrap();

        let err = claim_files(&cfg, &tmp, "runA", None, &files(&["src/a.rs"]), 100)
            .expect_err("an unreadable registry must not grant a claim");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("refusing to claim files"),
            "the error must say what was refused, got: {msg}"
        );

        assert_eq!(
            std::fs::read(&path).unwrap(),
            corrupt,
            "a refused claim must not overwrite the registry it could not parse"
        );
    }

    /// The other half: an ABSENT registry is a legitimately empty one, so the
    /// first claim in a fresh project still succeeds. Without this pin the fix
    /// above could degrade into a blanket refusal.
    #[test]
    fn absent_registry_is_empty_and_claim_succeeds() {
        let tmp = make_tmp_dir("absent-ok");
        let cfg = make_cfg(&tmp);
        let path = registry_path(&cfg, &tmp);
        assert!(!path.exists(), "precondition: no registry yet");

        let out = claim_files(&cfg, &tmp, "runA", None, &files(&["src/a.rs"]), 100).unwrap();
        assert_eq!(out.claimed, vec!["src/a.rs".to_string()]);
        assert!(load_known(&path).files.contains_key("src/a.rs"));
    }

    /// Release/heartbeat must not publish an empty registry over bytes they
    /// could not parse — that would turn a transient unreadable file into
    /// permanent loss of every other session's claims.
    #[test]
    fn corrupt_registry_refuses_release_and_heartbeat_without_clobbering() {
        let tmp = make_tmp_dir("corrupt-release");
        let cfg = make_cfg(&tmp);
        let path = registry_path(&cfg, &tmp);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let corrupt: &[u8] = b"{ truncated";
        std::fs::write(&path, corrupt).unwrap();

        assert!(release_run(&cfg, &tmp, "runA").is_err(), "release_run");
        assert!(
            release_files(&cfg, &tmp, "runA", &files(&["src/a.rs"])).is_err(),
            "release_files"
        );
        assert!(
            release_tasks(&cfg, &tmp, &files(&["hk1"])).is_err(),
            "release_tasks"
        );
        assert!(heartbeat(&cfg, &tmp, "runA", 100).is_err(), "heartbeat");
        assert!(
            claim_tasks(&cfg, &tmp, &files(&["hk1"]), "runA", None, 100, None).is_err(),
            "claim_tasks"
        );
        assert!(active_claims(&cfg, &tmp, 100).is_err(), "active_claims");
        assert!(
            write_execution_state(&cfg, &tmp, 100).is_err(),
            "write_execution_state"
        );

        assert_eq!(
            std::fs::read(&path).unwrap(),
            corrupt,
            "no refused operation may rewrite the unreadable registry"
        );
    }

    // ---- task-level claims ------------------------------------------------

    #[test]
    fn task_claim_succeeds_and_persists_with_title() {
        let tmp = make_tmp_dir("task-first");
        let cfg = make_cfg(&tmp);
        let out = claim_tasks(
            &cfg,
            &tmp,
            &files(&["hk-1"]),
            "runA",
            Some("sessA"),
            100,
            Some("do the thing"),
        )
        .unwrap();
        assert_eq!(out.claimed, vec!["hk-1".to_string()]);
        assert!(out.skipped.is_empty());
        let live = active_claims(&cfg, &tmp, 100).unwrap();
        assert_eq!(live.task_claims["hk-1"].run_id, "runA");
        assert_eq!(
            live.task_claims["hk-1"].title.as_deref(),
            Some("do the thing")
        );
        // Task claims live in their own table, not the file table.
        assert!(live.files.is_empty());
    }

    #[test]
    fn same_task_held_by_another_live_run_is_skipped() {
        let tmp = make_tmp_dir("task-conflict");
        let cfg = make_cfg(&tmp);
        claim_tasks(&cfg, &tmp, &files(&["hk-1"]), "runA", None, 100, None).unwrap();
        let out = claim_tasks(&cfg, &tmp, &files(&["hk-1"]), "runB", None, 101, None).unwrap();
        assert!(out.claimed.is_empty());
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.skipped[0].file, "hk-1");
        assert_eq!(out.skipped[0].holder_run, "runA");
        // Still owned by runA.
        let live = active_claims(&cfg, &tmp, 102).unwrap();
        assert_eq!(live.task_claims["hk-1"].run_id, "runA");
    }

    #[test]
    fn stale_task_claim_with_confirmed_stalled_progress_is_reaped_and_reclaimable() {
        let tmp = make_tmp_dir("task-stale");
        let cfg = make_cfg(&tmp);
        let mut reg = Registry::default();
        reg.task_claims.insert(
            "hk-1".to_string(),
            Claim {
                run_id: "ghost".into(),
                session_id: Some("dead".into()),
                pid: std::process::id(),
                claimed_at: 0,
                heartbeat_at: 0,
                title: Some("stale task".into()),
            },
        );
        save(&registry_path(&cfg, &tmp), &reg).unwrap();
        // now past the TTL of the heartbeat → eligible; confirmed Stalled → reaped.
        let now = cfg.stuck_ttl_secs as i64 + 1;
        let out = test_hook::with_forced(Determination::Known(Liveness::Stalled), || {
            // The reclaiming acquire AND the observability read both reap under a
            // Stalled verdict, so hold the seam across both.
            let out = claim_tasks(&cfg, &tmp, &files(&["hk-1"]), "runB", None, now, None).unwrap();
            let live = active_claims(&cfg, &tmp, now).unwrap();
            assert_eq!(live.task_claims["hk-1"].run_id, "runB");
            out
        });
        assert_eq!(out.claimed, vec!["hk-1".to_string()]);
        assert!(out.skipped.is_empty());
    }

    #[test]
    fn stale_task_claim_but_progressing_is_not_reaped() {
        let tmp = make_tmp_dir("task-stale-progressing");
        let cfg = make_cfg(&tmp);
        let mut reg = Registry::default();
        reg.task_claims.insert(
            "hk-1".to_string(),
            Claim {
                run_id: "busy".into(),
                session_id: Some("live".into()),
                pid: std::process::id(),
                claimed_at: 0,
                heartbeat_at: 0,
                title: Some("still working".into()),
            },
        );
        save(&registry_path(&cfg, &tmp), &reg).unwrap();
        let now = cfg.stuck_ttl_secs as i64 + 1;
        let out = test_hook::with_forced(Determination::Known(Liveness::Progressing), || {
            claim_tasks(&cfg, &tmp, &files(&["hk-1"]), "runB", None, now, None).unwrap()
        });
        assert!(
            out.claimed.is_empty(),
            "progressing task holder must be kept"
        );
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.skipped[0].holder_run, "busy");
    }

    #[test]
    fn release_tasks_frees_the_hashkey() {
        let tmp = make_tmp_dir("task-release");
        let cfg = make_cfg(&tmp);
        claim_tasks(&cfg, &tmp, &files(&["hk-1"]), "runA", None, 100, None).unwrap();
        let n = release_tasks(&cfg, &tmp, &files(&["hk-1"])).unwrap();
        assert_eq!(n, 1);
        // Now runB can take it.
        let out = claim_tasks(&cfg, &tmp, &files(&["hk-1"]), "runB", None, 101, None).unwrap();
        assert_eq!(out.claimed, vec!["hk-1".to_string()]);
    }

    #[test]
    fn release_run_drops_both_file_and_task_claims() {
        let tmp = make_tmp_dir("task-release-run");
        let cfg = make_cfg(&tmp);
        claim_files(&cfg, &tmp, "runA", None, &files(&["src/a.rs"]), 100).unwrap();
        claim_tasks(&cfg, &tmp, &files(&["hk-1"]), "runA", None, 100, None).unwrap();
        let n = release_run(&cfg, &tmp, "runA").unwrap();
        assert_eq!(n, 2, "one file + one task claim released");
        let live = active_claims(&cfg, &tmp, 101).unwrap();
        assert!(live.files.is_empty());
        assert!(live.task_claims.is_empty());
    }

    #[test]
    fn heartbeat_refreshes_both_file_and_task_claims() {
        let tmp = make_tmp_dir("task-hb");
        let cfg = make_cfg(&tmp);
        claim_files(&cfg, &tmp, "runA", None, &files(&["src/a.rs"]), 0).unwrap();
        claim_tasks(&cfg, &tmp, &files(&["hk-1"]), "runA", None, 0, None).unwrap();
        let n = heartbeat(&cfg, &tmp, "runA", 500).unwrap();
        assert_eq!(n, 2, "both the file and the task claim refreshed");
        let live = active_claims(&cfg, &tmp, 500).unwrap();
        assert_eq!(live.files["src/a.rs"].heartbeat_at, 500);
        assert_eq!(live.task_claims["hk-1"].heartbeat_at, 500);
    }

    // ---- execution-state view --------------------------------------------

    #[test]
    fn execution_join_emits_status_and_claimed_by() {
        // Pure join: a claim whose title matches a pending backlog row gets enriched.
        let mut task_claims = BTreeMap::new();
        task_claims.insert(
            "hk-1".to_string(),
            Claim {
                run_id: "runA".into(),
                session_id: Some("sessA".into()),
                pid: 1,
                claimed_at: 10,
                heartbeat_at: 42,
                title: Some("wire the CLI".into()),
            },
        );
        // An unmatched claim (no backlog row) must still appear, with null backlog.
        task_claims.insert(
            "hk-2".to_string(),
            Claim {
                run_id: "runB".into(),
                session_id: None,
                pid: 2,
                claimed_at: 10,
                heartbeat_at: 43,
                title: Some("orphan task".into()),
            },
        );
        let pending = vec![serde_json::json!({
            "id": 77,
            "title": "wire the CLI",
            "status": "pending",
            "project": "condukt",
        })];
        let rows = join_execution_state(&task_claims, &pending);
        assert_eq!(rows.len(), 2);
        // BTreeMap order → hk-1 first.
        let r0 = &rows[0];
        assert_eq!(r0.hashkey, "hk-1");
        assert_eq!(r0.backlog_id.as_deref(), Some("77"));
        assert_eq!(r0.status.as_deref(), Some("pending"));
        assert_eq!(r0.project.as_deref(), Some("condukt"));
        assert_eq!(r0.title.as_deref(), Some("wire the CLI"));
        assert_eq!(r0.claimed_by.run_id, "runA");
        assert_eq!(r0.claimed_by.session_id.as_deref(), Some("sessA"));
        assert_eq!(r0.claimed_by.heartbeat_at, 42);
        // Unmatched row: claim data present, backlog fields absent.
        let r1 = &rows[1];
        assert_eq!(r1.hashkey, "hk-2");
        assert!(r1.backlog_id.is_none());
        assert!(r1.status.is_none());
        assert_eq!(r1.claimed_by.run_id, "runB");
    }

    #[test]
    fn write_execution_state_is_fail_soft_without_backlog() {
        // With no `backlog` binary on PATH the join yields null backlog fields, but
        // the held claims are still written and returned.
        let tmp = make_tmp_dir("exec-failsoft");
        let cfg = make_cfg(&tmp);
        claim_tasks(
            &cfg,
            &tmp,
            &files(&["hk-1"]),
            "runA",
            Some("sessA"),
            100,
            Some("a task"),
        )
        .unwrap();
        let rows = write_execution_state(&cfg, &tmp, 100).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].hashkey, "hk-1");
        assert_eq!(rows[0].claimed_by.run_id, "runA");
        assert_eq!(rows[0].title.as_deref(), Some("a task"));
        // The file was written beside claims.json and round-trips.
        let path = execution_state_path(&cfg, &tmp);
        let txt = std::fs::read_to_string(&path).unwrap();
        let parsed: Vec<ExecutionEntry> = serde_json::from_str(&txt).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].hashkey, "hk-1");
    }

    // A wedged holder of the claims-registry lock must make the REAL production
    // claim path HARD-SKIP (report every file skipped, claim none, persist
    // nothing) rather than degrade to an unlocked read-modify-write that could
    // double-claim. RED with the fail-soft `RunLock::acquire` this crate used to
    // expose (since REMOVED): it handed back an unlocked guard, the RMW
    // proceeded, and the files were claimed (claimed non-empty, registry
    // written). GREEN with `acquire_or_skip_with_deadline`.
    #[test]
    fn contended_claims_lock_makes_claim_files_hard_skip_not_double_claim() {
        use std::time::Duration;
        let tmp = make_tmp_dir("wedge-skip");
        let cfg = make_cfg(&tmp);

        // Wedge a live holder of the reserved claims lock for the whole test.
        let held = RunLock::acquire_or_skip(&cfg, &tmp, CLAIMS_LOCK_KEY)
            .expect("precondition: wedge must genuinely hold the claims lock");
        assert!(
            held.held(),
            "precondition: wedge must genuinely hold the claims lock"
        );

        // The real production path (claim_files -> claim_files_with_deadline)
        // must skip every file under a short deadline rather than proceed
        // unlocked and double-claim.
        let out = claim_files_with_deadline(
            &cfg,
            &tmp,
            "runB",
            Some("sessB"),
            &files(&["src/a.rs", "src/b.rs"]),
            100,
            Duration::from_millis(120),
        )
        .unwrap();

        assert!(
            out.claimed.is_empty(),
            "contended claim must claim NOTHING, got: {:?}",
            out.claimed
        );
        assert_eq!(
            out.skipped.len(),
            2,
            "contended claim must report every requested file skipped"
        );
        assert!(
            out.skipped
                .iter()
                .all(|s| s.holder_run == LOCK_CONTENDED_HOLDER),
            "skips must be marked as lock-contention skips"
        );

        // Nothing was written to the registry (no unlocked double-claim).
        let path = registry_path(&cfg, &tmp);
        assert!(
            !path.exists() || load_known(&path).files.is_empty(),
            "a contended claim must not persist any claim to the registry"
        );

        drop(held);
    }

    #[test]
    fn old_bare_map_claims_json_still_loads() {
        // Backward compat: a pre-task-claims registry was a bare file->claim map.
        // The flattened `files` field must absorb it.
        let tmp = make_tmp_dir("backcompat");
        let cfg = make_cfg(&tmp);
        let path = registry_path(&cfg, &tmp);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            br#"{"src/a.rs":{"run_id":"runA","pid":1,"claimed_at":5,"heartbeat_at":5}}"#,
        )
        .unwrap();
        let live = active_claims(&cfg, &tmp, 6).unwrap();
        assert_eq!(live.files["src/a.rs"].run_id, "runA");
        assert!(live.task_claims.is_empty());
    }

    // ── the reap gate's head signal must be RUN-scoped, not repo-wide ───────
    //
    // [`claim_progress`] folds three durable signals into one fingerprint, and
    // [`retain_claim`] reaps a heartbeat-stale claim ONLY on `Known(Stalled)`.
    // Signal 1 is today `progress::git_head_signal(&repo_root(cwd))` — the
    // WHOLE project repo's HEAD. Under CLAUDE.md §8 other sessions always exist
    // and always commit to that repo, so this signal advances for reasons that
    // have nothing to do with the claim's holder: the fingerprint never
    // freezes, the verdict never reaches `Stalled`, and a DEAD holder's claim
    // can never be reclaimed. That is a fail-open — a permanently un-reapable
    // claim — and it is the twin of the already-fixed `state::probe_run` defect
    // (commit f906094c), except that this one holds reap authority.
    //
    // These tests exercise the REAL signal path: none of them uses
    // `test_hook::with_forced`, which early-returns before a single signal is
    // read and would therefore prove nothing about the scoping. The other two
    // signals (session transcript, run max `updated_at`) are materialised and
    // held FROZEN so the head signal is the only thing that can move the
    // fingerprint — that is what makes each assertion discriminating.
    //
    //   A1   run worktree FROZEN, project repo HEAD MOVES  ⇒ Known(Stalled)
    //   A4i  run worktree MOVES,  project repo HEAD FROZEN ⇒ Known(Progressing)
    //   A4ii run records NO task worktree                  ⇒ Undetermined, kept
    //   A3   run's recorded worktree is unreadable         ⇒ Undetermined, kept

    /// `$HOME` pinned to `home` for as long as the guard lives, serialized
    /// against every other `$HOME` mutator in this crate via
    /// `crate::env_lock::HOME_ENV_LOCK`. Restoration happens in `Drop`, so a
    /// FAILING assertion (these tests are expected to fail before the fix)
    /// cannot leave the process pointing at a temp home and poison the rest of
    /// the suite.
    struct HomeGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: Option<std::ffi::OsString>,
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    fn pin_home(home: &Path) -> HomeGuard {
        let lock = crate::env_lock::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", home);
        HomeGuard { _lock: lock, prev }
    }

    /// Minimal git repo with one commit and a repo-LOCAL identity (so commits
    /// work regardless of what `$HOME` is pinned to).
    fn init_git_repo(dir: &Path) {
        use crate::worktree::git;
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-b", "main"]).unwrap();
        git(dir, &["config", "user.email", "test@example.com"]).unwrap();
        git(dir, &["config", "user.name", "Test"]).unwrap();
        std::fs::write(dir.join("base.txt"), "base\n").unwrap();
        git(dir, &["add", "."]).unwrap();
        git(dir, &["commit", "-m", "init"]).unwrap();
    }

    fn head_of(repo: &Path) -> String {
        crate::worktree::git(repo, &["rev-parse", "HEAD"])
            .unwrap_or_else(|e| panic!("git rev-parse HEAD in {}: {e}", repo.display()))
            .trim()
            .to_string()
    }

    /// Advance `repo`'s HEAD with one empty commit, asserting the sha actually
    /// moved. A git that silently did nothing must fail loudly here rather than
    /// read downstream as "nothing advanced".
    fn git_advance_head(repo: &Path) {
        let before = head_of(repo);
        crate::worktree::git(repo, &["commit", "--allow-empty", "-m", "advance"])
            .unwrap_or_else(|e| panic!("git commit --allow-empty in {}: {e}", repo.display()));
        assert_ne!(
            before,
            head_of(repo),
            "fixture precondition: an empty commit must move HEAD in {}",
            repo.display()
        );
    }

    /// The session id every fixture below uses; its transcript is seeded once
    /// and never rewritten, so the transcript signal is READABLE but frozen.
    const FIXTURE_SESSION: &str = "sess-fixture";

    /// Build the claim-progress fixture under `tmp` and return `(cfg, claim,
    /// home)`:
    ///
    /// * `tmp` is itself a git repo, so `repo_root(tmp) == tmp` — the repo-wide
    ///   HEAD the CURRENT code samples is one the test can move at will.
    /// * a run `run_id` with exactly one RUNNING task whose `worktree` field is
    ///   `worktree` verbatim (this function does NOT create it — a caller that
    ///   wants a real one calls [`init_git_repo`] itself, which is how the
    ///   "recorded but unreadable" case is expressed) and whose `updated_at` is
    ///   FROZEN at a fixed value.
    /// * a frozen session transcript at `<home>/.claude/projects/*/<sid>.jsonl`.
    /// * a heartbeat-stale [`Claim`] owned by that run (`heartbeat_at: 0`), so
    ///   [`retain_claim`] actually reaches the progress verdict.
    fn progress_fixture(
        tmp: &Path,
        run_id: &str,
        worktree: Option<&Path>,
    ) -> (Config, Claim, PathBuf) {
        init_git_repo(tmp);
        // Observed, not assumed: this is the path the CURRENT code hands to
        // `git_head_signal`, so the test genuinely controls that signal.
        assert_eq!(
            repo_root(tmp),
            tmp.to_path_buf(),
            "fixture precondition: repo_root(cwd) must resolve to the fixture repo"
        );

        let home = tmp.join("home");
        let proj = home.join(".claude").join("projects").join("-fixture");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join(format!("{FIXTURE_SESSION}.jsonl")),
            "{\"seed\":1}\n",
        )
        .unwrap();

        let cfg = make_cfg(tmp);
        let rs = crate::state::RunState {
            run_id: run_id.to_string(),
            goal: "g".into(),
            tasks: vec![crate::state::TaskState {
                id: "t-only".into(),
                status: crate::state::Status::Running,
                worktree: worktree.map(|p| p.to_string_lossy().into_owned()),
                // Frozen for the whole test: the OTHER durable signal must not
                // move for a legitimate reason, or the test proves nothing.
                updated_at: Some(9_000),
                ..Default::default()
            }],
            paused: false,
            terminal_label: None,
            recorded_at: None,
        };
        rs.save(&cfg, tmp).unwrap();

        let claim = Claim {
            run_id: run_id.to_string(),
            session_id: Some(FIXTURE_SESSION.to_string()),
            pid: 424_242,
            claimed_at: 0,
            heartbeat_at: 0,
            title: None,
        };
        (cfg, claim, home)
    }

    /// (A1) The defect, stated as the verdict it must produce. The run's own
    /// worktree HEAD and its task `updated_at` are frozen for a full window
    /// while ANOTHER session commits to the shared project repo. A commit made
    /// elsewhere in the repo is not evidence that THIS run advanced, so the
    /// verdict must converge to `Known(Stalled)` — the only value that lets a
    /// dead holder's claim ever be reclaimed.
    #[test]
    fn claim_progress_stalled_when_run_worktree_frozen_though_repo_head_advances() {
        let tmp = make_tmp_dir("claim-scope-stall");
        let wt = tmp.join("run-worktree");
        init_git_repo(&wt);
        let (cfg, claim, home) = progress_fixture(&tmp, "runFrozen", Some(&wt));
        assert_eq!(
            repo_root(&wt),
            wt,
            "fixture precondition: the run's worktree must be its own git root"
        );
        let _h = pin_home(&home);
        let window = progress::window_secs(progress::DEFAULT_WINDOW_SECS);
        let t0 = 10_000i64;

        // Sample 1 anchors the fingerprint. With no prior snapshot this is
        // Undetermined by construction; if it is not, the fixture is broken
        // (e.g. a signal was unreadable in a way the test did not intend).
        let v1 = claim_progress(&cfg, &tmp, &claim, t0);
        assert!(
            matches!(v1, Determination::Undetermined(_)),
            "first sample (no prior snapshot) must be Undetermined, got {v1:?}"
        );

        // A DIFFERENT session commits to the shared project repo.
        let wt_before = head_of(&wt);
        git_advance_head(&tmp);
        assert_eq!(
            wt_before,
            head_of(&wt),
            "fixture precondition: the run's own worktree must stay frozen"
        );

        let v2 = claim_progress(&cfg, &tmp, &claim, t0 + window);
        assert_eq!(
            v2,
            Determination::Known(Liveness::Stalled),
            "the run's own worktree and its task updated_at were frozen for a full \
             window; a commit made elsewhere in the shared project repo is not \
             evidence that THIS run advanced"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// (A4-i) The arm where the run HAS a task worktree: that worktree's HEAD
    /// is the run-scoped head signal, so a real commit inside it reads as
    /// `Known(Progressing)` even though the shared project repo is frozen.
    /// This is the anti-vacuity control for (A1): "always answer Stalled" is
    /// not an acceptable fix, because it would force-steal a live holder.
    #[test]
    fn claim_progress_progressing_when_run_worktree_commits_though_repo_head_frozen() {
        let tmp = make_tmp_dir("claim-scope-progress");
        let wt = tmp.join("run-worktree");
        init_git_repo(&wt);
        let (cfg, claim, home) = progress_fixture(&tmp, "runBusy", Some(&wt));
        let _h = pin_home(&home);
        let window = progress::window_secs(progress::DEFAULT_WINDOW_SECS);
        let t0 = 10_000i64;

        let v1 = claim_progress(&cfg, &tmp, &claim, t0);
        assert!(
            matches!(v1, Determination::Undetermined(_)),
            "first sample (no prior snapshot) must be Undetermined, got {v1:?}"
        );

        // The holder does durable work in its OWN worktree; nothing else
        // commits to the shared project repo.
        let repo_before = head_of(&tmp);
        git_advance_head(&wt);
        assert_eq!(
            repo_before,
            head_of(&tmp),
            "fixture precondition: the shared project repo HEAD must stay frozen"
        );

        let v2 = claim_progress(&cfg, &tmp, &claim, t0 + window);
        assert_eq!(
            v2,
            Determination::Known(Liveness::Progressing),
            "the holder committed inside the run's own worktree, so it is \
             demonstrably alive and its claim must never be reaped"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// (A4-ii) The arm where the run records NO task worktree at all (serial /
    /// fast-path / single-worktree mode). There is no run-scoped durable head
    /// signal to read, which is "cannot determine" — NOT "frozen", and NOT a
    /// fall back to the repo-wide HEAD (that fallback is the defect itself).
    /// Asserted behaviourally at the reap layer too, because `Stalled` is the
    /// only verdict that fires a reap: writing "cannot read" as "frozen" would
    /// steal the claim.
    #[test]
    fn claim_progress_without_run_worktree_is_undetermined_and_keeps_the_claim() {
        let tmp = make_tmp_dir("claim-scope-no-worktree");
        let (cfg, claim, home) = progress_fixture(&tmp, "runNoWt", None);
        let _h = pin_home(&home);
        let window = progress::window_secs(progress::DEFAULT_WINDOW_SECS);
        let t0 = 10_000i64;

        // Nothing moves anywhere: the shared repo HEAD is frozen too, so the
        // ONLY thing that can keep this out of `Stalled` is refusing to answer.
        let repo_head = head_of(&tmp);
        let _ = claim_progress(&cfg, &tmp, &claim, t0);
        let now = t0 + window;
        let v2 = claim_progress(&cfg, &tmp, &claim, now);
        assert_eq!(
            repo_head,
            head_of(&tmp),
            "fixture precondition: nothing may commit during this test"
        );
        assert!(
            matches!(v2, Determination::Undetermined(_)),
            "a run with no recorded task worktree has no run-scoped head signal \
             at all; 'cannot determine' must not be written as 'frozen' (got {v2:?})"
        );

        // The safety property, not just the enum value: the reap must not fire.
        let kept = retain_claim(&claim, now, ttl_secs(&cfg), &|c| {
            claim_progress(&cfg, &tmp, c, now)
        });
        assert!(
            kept,
            "a heartbeat-stale claim whose run-scoped progress is Undetermined \
             must be KEPT — only a confirmed Known(Stalled) may reap"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// (A3) The protective-Undetermined pin: the run DOES record a worktree, the
    /// directory is THERE, but its git state cannot be read (malformed `.git`).
    /// An unreadable signal must resolve to `Undetermined`, never to
    /// `Known(Stalled)` — and the consequence at the reap layer is that the
    /// claim survives.
    ///
    /// Re-anchored for 5e62b0eb. The named property (unreadable ⇒ Undetermined ⇒
    /// claim kept) and both assertions are UNCHANGED; only the fixture moved.
    /// It used to record a purely ABSENT path, using absence as a PROXY for
    /// unreadability — a proxy that was exact only while the implementation
    /// conflated the two. 5e62b0eb draws the line exactly between them (absence
    /// is a successful observation; "there but unaskable" is not), so the proxy
    /// has drifted off the property this test names, and the fixture now
    /// expresses unreadability directly. The absent case is covered by
    /// `claim_with_only_absent_run_worktrees_is_reaped_once_stale`, which
    /// asserts the OPPOSITE outcome.
    #[test]
    fn claim_progress_with_unreadable_run_worktree_is_undetermined_and_keeps_the_claim() {
        let tmp = make_tmp_dir("claim-scope-unreadable-wt");
        // Recorded in run state and really present on disk, but git refuses to
        // answer about it: a genuine failure to observe, not an observation.
        let gone = tmp.join("worktree-that-is-not-a-repo");
        make_unreadable_worktree(&gone);
        let (cfg, claim, home) = progress_fixture(&tmp, "runGoneWt", Some(&gone));
        assert!(
            gone.is_dir(),
            "fixture precondition: the recorded worktree must exist"
        );
        assert!(
            matches!(
                progress::git_head_signal(&gone),
                Determination::Undetermined(_)
            ),
            "fixture precondition: git must genuinely refuse on the recorded \
             worktree, or this test guards nothing"
        );
        let _h = pin_home(&home);
        let window = progress::window_secs(progress::DEFAULT_WINDOW_SECS);
        let t0 = 10_000i64;

        let repo_head = head_of(&tmp);
        let _ = claim_progress(&cfg, &tmp, &claim, t0);
        let now = t0 + window;
        let v2 = claim_progress(&cfg, &tmp, &claim, now);
        assert_eq!(
            repo_head,
            head_of(&tmp),
            "fixture precondition: nothing may commit during this test"
        );
        assert!(
            matches!(v2, Determination::Undetermined(_)),
            "an unreadable run-scoped head signal is 'cannot determine'; reading \
             it as 'frozen' would hand reap authority to a failed read (got {v2:?})"
        );

        let kept = retain_claim(&claim, now, ttl_secs(&cfg), &|c| {
            claim_progress(&cfg, &tmp, c, now)
        });
        assert!(
            kept,
            "a heartbeat-stale claim whose run-scoped head signal could not be \
             read must be KEPT, never force-stolen"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    // ── the run-scoped head signal's two multi-worktree properties ─────────
    //
    // `run_worktree_head_signal` folds every worktree its run records into one
    // value, and it does two things to make that fold well-defined:
    //
    //   (1) it sorts the worktrees by task id, so the run state's task ORDER is
    //       not an input; and
    //   (2) it folds each HEAD together with the task id that owns it, so a
    //       HEAD is bound to a task rather than floating free in a sequence.
    //
    // Both were unprotected: with the existing fixture every run records
    // exactly ONE worktree, so neither ordering nor identity can show up at the
    // observation point. Measured independently at 639813db by deleting each
    // construct — `worktrees.sort_by(...)` removed, and the task id dropped
    // from the fold — the whole 914-test suite stayed green both times (backlog
    // 22a72fdf). A construct no test can kill is a judgment, not an
    // observation, so each gets its own discriminating test below.
    //
    // Both go through `claim_progress`, not the helper, because that is where
    // reap authority lives, and both hold the OTHER two signals frozen (the
    // transcript is seeded once; every `updated_at` is pinned to 9_000) so the
    // head signal is the only thing that can move the fingerprint.

    /// Rewrite `run_id`'s state so that exactly `entries` record a worktree, in
    /// the given order, with every `updated_at` FROZEN. The freeze is what
    /// makes these tests discriminating: if the run-tasks signal could move for
    /// a legitimate reason, a verdict change would prove nothing about the head
    /// signal.
    fn save_run_worktrees(cfg: &Config, cwd: &Path, run_id: &str, entries: &[(&str, &Path)]) {
        let rs = crate::state::RunState {
            run_id: run_id.to_string(),
            goal: "g".into(),
            tasks: entries
                .iter()
                .map(|(id, wt)| crate::state::TaskState {
                    id: (*id).to_string(),
                    status: crate::state::Status::Running,
                    worktree: Some(wt.to_string_lossy().into_owned()),
                    updated_at: Some(9_000),
                    ..Default::default()
                })
                .collect(),
            paused: false,
            terminal_label: None,
            recorded_at: None,
        };
        rs.save(cfg, cwd).unwrap();
    }

    /// (B1) Property (1): the run state's task ORDER must not be an input.
    ///
    /// Two worktrees, both frozen, and between the two samples the run state is
    /// rewritten with its tasks in the opposite order — a pure reordering, no
    /// commit anywhere, no `updated_at` change. Nothing advanced, so the
    /// verdict must be `Known(Stalled)`.
    ///
    /// Without the `sort_by`, the fold walks the run state's own task order, a
    /// reordering alone changes the fingerprint, and a genuinely frozen holder
    /// reads as `Progressing` — an un-reapable claim, which is the very
    /// fail-open this fix exists to close, re-entering through the back door.
    #[test]
    fn claim_progress_head_signal_ignores_run_state_task_ordering() {
        let tmp = make_tmp_dir("claim-scope-order");
        let wt_a = tmp.join("wt-a");
        let wt_b = tmp.join("wt-b");
        init_git_repo(&wt_a);
        init_git_repo(&wt_b);
        // Two repos initialised in the same second produce the IDENTICAL commit
        // sha (same tree, message, author and timestamp), so B needs one extra
        // commit to be distinguishable at all.
        git_advance_head(&wt_b);
        assert_ne!(
            head_of(&wt_a),
            head_of(&wt_b),
            "fixture precondition: the two worktrees must have distinct HEADs, \
             or a reordering could not change the fold even without the sort"
        );
        let (cfg, claim, home) = progress_fixture(&tmp, "runOrder", Some(&wt_a));
        save_run_worktrees(&cfg, &tmp, "runOrder", &[("t-a", &wt_a), ("t-b", &wt_b)]);
        let _h = pin_home(&home);
        let window = progress::window_secs(progress::DEFAULT_WINDOW_SECS);
        let t0 = 10_000i64;

        let v1 = claim_progress(&cfg, &tmp, &claim, t0);
        assert!(
            matches!(v1, Determination::Undetermined(_)),
            "first sample (no prior snapshot) must be Undetermined, got {v1:?}"
        );

        let (repo_head, a_head, b_head) = (head_of(&tmp), head_of(&wt_a), head_of(&wt_b));
        // The ONLY change: the same two tasks, written in the opposite order.
        save_run_worktrees(&cfg, &tmp, "runOrder", &[("t-b", &wt_b), ("t-a", &wt_a)]);

        let v2 = claim_progress(&cfg, &tmp, &claim, t0 + window);
        assert_eq!(
            (repo_head, a_head, b_head),
            (head_of(&tmp), head_of(&wt_a), head_of(&wt_b)),
            "fixture precondition: no HEAD may move during this test"
        );
        assert_eq!(
            v2,
            Determination::Known(Liveness::Stalled),
            "reordering the run state's task list is not durable work; if task \
             order is an input to the head signal, a frozen holder never reaches \
             Stalled and its claim can never be reclaimed"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// (B2) Property (2): each HEAD is bound to the task id that owns it.
    ///
    /// The run's single worktree-bearing task changes from `t-a` (worktree A)
    /// to `t-b` (worktree B) — and B is a CLONE of A, so it sits on the
    /// identical commit sha. The run's worktree composition changed, which is
    /// durable work, so the verdict must be `Known(Progressing)` and the claim
    /// must survive.
    ///
    /// Without the task id in the fold both samples reduce to the same lone
    /// sha, the run reads as frozen, and the reap force-steals the claim of a
    /// holder that had just moved its work into a new worktree. A fresh condukt
    /// worktree is branched from the same base commit, so "different task, same
    /// HEAD" is the normal case here, not a contrived one.
    #[test]
    fn claim_progress_head_signal_binds_each_head_to_its_owning_task() {
        let tmp = make_tmp_dir("claim-scope-identity");
        let wt_a = tmp.join("wt-a");
        let wt_b = tmp.join("wt-b");
        init_git_repo(&wt_a);
        crate::worktree::git(
            &tmp,
            &["clone", &wt_a.to_string_lossy(), &wt_b.to_string_lossy()],
        )
        .unwrap_or_else(|e| panic!("git clone {} -> {}: {e}", wt_a.display(), wt_b.display()));
        assert_eq!(
            head_of(&wt_a),
            head_of(&wt_b),
            "fixture precondition: the clone must sit on the IDENTICAL commit, \
             or the sha alone would already distinguish the two samples"
        );

        let (cfg, claim, home) = progress_fixture(&tmp, "runIdent", Some(&wt_a));
        save_run_worktrees(&cfg, &tmp, "runIdent", &[("t-a", &wt_a)]);
        let _h = pin_home(&home);
        let window = progress::window_secs(progress::DEFAULT_WINDOW_SECS);
        let t0 = 10_000i64;

        let v1 = claim_progress(&cfg, &tmp, &claim, t0);
        assert!(
            matches!(v1, Determination::Undetermined(_)),
            "first sample (no prior snapshot) must be Undetermined, got {v1:?}"
        );

        let repo_head = head_of(&tmp);
        // The run's work moves to a different task, whose fresh worktree is
        // branched from the same base commit.
        save_run_worktrees(&cfg, &tmp, "runIdent", &[("t-b", &wt_b)]);

        let now = t0 + window;
        let v2 = claim_progress(&cfg, &tmp, &claim, now);
        assert_eq!(
            repo_head,
            head_of(&tmp),
            "fixture precondition: nothing may commit to the shared repo here"
        );
        assert_eq!(
            v2,
            Determination::Known(Liveness::Progressing),
            "the run swapped its recorded worktree for a different task's; if a \
             HEAD is not bound to its owning task, two different worktrees on \
             the same base commit alias and the holder reads as frozen (got {v2:?})"
        );

        // The consequence, not just the enum value.
        let kept = retain_claim(&claim, now, ttl_secs(&cfg), &|c| {
            claim_progress(&cfg, &tmp, c, now)
        });
        assert!(
            kept,
            "a holder whose run-scoped worktree set changed is demonstrably \
             alive; its claim must never be force-stolen"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    // ── absence is an OBSERVATION, not a failure to observe (5e62b0eb) ──────
    //
    // `run_worktree_head_signal` folds each recorded worktree's git HEAD, and
    // today it collapses TWO different facts into one `Undetermined`:
    //
    //   * the directory is GONE — `stat()` returned ENOENT, which is a
    //     SUCCESSFUL observation. git can never advance a HEAD in a directory
    //     that does not exist, so a run whose worktrees are all absent will
    //     never progress again; and
    //   * the directory is THERE but git could not be asked (malformed/absent
    //     `.git`, permissions, transient IO) — a genuine failure to observe.
    //
    // Only the second is "cannot determine" (CLAUDE.md §3). Conflating them
    // makes a run whose worktrees have been deleted read `Undetermined`
    // forever, so `retain_claim` keeps its claim forever — measured live, 10
    // file claims held 40.4 days and 9 task claims held up to 57.5 days against
    // an 1800s TTL.
    //
    // The three tests below pin the split, and they state the property at the
    // REAP layer (did the claim actually leave the registry?) rather than only
    // at the verdict enum, because reap authority is what the defect abuses.
    // (C2) and (C3) are the SAFETY side and take precedence over (C1) in any
    // tradeoff: a live run must never be wrongly reaped.

    /// Build a directory that EXISTS but whose git state cannot be read.
    ///
    /// A plain empty directory is NOT enough: `git -C <dir> rev-parse HEAD`
    /// walks UP, so a bare directory inside the fixture repo answers with the
    /// ENCLOSING repo's HEAD (observed: exit 0, the outer sha). A `.git` that is
    /// present but malformed makes git refuse — `fatal: invalid gitfile format`,
    /// exit 128 — while `stat()` on the directory still succeeds. That is
    /// exactly "there, but cannot be asked", as distinct from "not there".
    fn make_unreadable_worktree(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(".git"), "this is not a gitfile\n").unwrap();
    }

    /// Hold `claim` under both a file key and a task key, run the REAL reap with
    /// the REAL progress path (no `test_hook::with_forced` — forcing the verdict
    /// would bypass the logic under test), and report whether each survived.
    ///
    /// Also asserts the claim is genuinely heartbeat-stale at `now`: without
    /// that, `retain_claim` short-circuits to "keep" before ever consulting
    /// progress, and a "the claim survived" assertion would pass vacuously.
    fn survives_reap(cfg: &Config, cwd: &Path, claim: &Claim, now: i64) -> (bool, bool) {
        let ttl = ttl_secs(cfg);
        assert!(
            is_stale(claim, now, ttl),
            "fixture precondition: the claim must be heartbeat-stale at {now} \
             (ttl {ttl}), or retain_claim never reaches the progress verdict and \
             the test proves nothing"
        );
        let mut reg = Registry::default();
        reg.files.insert("src/x.rs".to_string(), claim.clone());
        reg.task_claims
            .insert("task-hash".to_string(), claim.clone());
        reap(&mut reg, now, ttl, &|c| claim_progress(cfg, cwd, c, now));
        (
            reg.files.contains_key("src/x.rs"),
            reg.task_claims.contains_key("task-hash"),
        )
    }

    /// (C1) REAP SIDE. Every worktree the run records is ABSENT. No git HEAD
    /// anywhere can ever move for this run again, so with a lapsed heartbeat and
    /// a full sampling window elapsed the verdict must converge to
    /// `Known(Stalled)` and the claim must actually leave the registry.
    ///
    /// Two absent worktrees rather than one, so the absent marker has to be
    /// well-defined PER worktree and stable across samples; a marker that
    /// varied between samples would read as `Progressing` and keep the claim
    /// immortal in a different way.
    #[test]
    fn claim_with_only_absent_run_worktrees_is_reaped_once_stale() {
        let tmp = make_tmp_dir("claim-absent-reaped");
        let gone_a = tmp.join("wt-gone-a");
        let gone_b = tmp.join("wt-gone-b");
        let (cfg, claim, home) = progress_fixture(&tmp, "runAllGone", Some(&gone_a));
        save_run_worktrees(
            &cfg,
            &tmp,
            "runAllGone",
            &[("t-a", &gone_a), ("t-b", &gone_b)],
        );
        // Observed, not assumed: both recorded worktrees really are absent.
        assert!(
            !gone_a.exists() && !gone_b.exists(),
            "fixture precondition: both recorded worktrees must be absent"
        );
        let _h = pin_home(&home);
        let window = progress::window_secs(progress::DEFAULT_WINDOW_SECS);
        let t0 = 10_000i64;

        let v1 = claim_progress(&cfg, &tmp, &claim, t0);
        assert!(
            matches!(v1, Determination::Undetermined(_)),
            "first sample (no prior snapshot) must be Undetermined, got {v1:?}"
        );

        let repo_head = head_of(&tmp);
        let now = t0 + window;
        let v2 = claim_progress(&cfg, &tmp, &claim, now);
        assert_eq!(
            repo_head,
            head_of(&tmp),
            "fixture precondition: nothing may commit during this test"
        );
        assert_eq!(
            v2,
            Determination::Known(Liveness::Stalled),
            "every worktree this run records is GONE; stat() returning ENOENT is \
             a successful observation that git can never advance a HEAD there \
             again, so this is a determinate freeze — not 'cannot determine' \
             (got {v2:?})"
        );

        // The property, not the enum: the dead run's claim must be reclaimable.
        let (file_kept, task_kept) = survives_reap(&cfg, &tmp, &claim, now);
        assert!(
            !file_kept && !task_kept,
            "a heartbeat-stale claim whose run's worktrees are all absent must be \
             REAPED (file kept={file_kept}, task kept={task_kept}); keeping it is \
             how 10 file claims survived 40.4 days against an 1800s TTL"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// (C2) SAFETY SIDE — takes precedence over (C1). The run records one ABSENT
    /// worktree and one that EXISTS and is being COMMITTED to. The live one is
    /// proof the holder is alive, so the verdict must be `Known(Progressing)`
    /// and the claim must survive.
    ///
    /// This is the test that forbids the cheap fix. Folding absence in by simply
    /// OMITTING the absent worktree would leave only the live one — which here
    /// happens to work — but it also means a run with one absent and one live
    /// worktree silently degrades to the live one, and a run with ALL worktrees
    /// absent folds to a constant empty value that looks frozen for the wrong
    /// reason. The absent worktree must contribute an EXPLICIT absent marker so
    /// the live HEAD next to it can still move the fingerprint.
    #[test]
    fn claim_with_one_absent_and_one_advancing_worktree_is_not_reaped() {
        let tmp = make_tmp_dir("claim-absent-plus-live");
        let gone = tmp.join("wt-gone");
        let live = tmp.join("wt-live");
        init_git_repo(&live);
        let (cfg, claim, home) = progress_fixture(&tmp, "runMixed", Some(&live));
        save_run_worktrees(
            &cfg,
            &tmp,
            "runMixed",
            &[("t-gone", &gone), ("t-live", &live)],
        );
        assert!(
            !gone.exists(),
            "fixture precondition: the first recorded worktree must be absent"
        );
        assert_eq!(
            repo_root(&live),
            live,
            "fixture precondition: the live worktree must be its own git root, \
             or its HEAD is not a signal this test controls"
        );
        let _h = pin_home(&home);
        let window = progress::window_secs(progress::DEFAULT_WINDOW_SECS);
        let t0 = 10_000i64;

        let v1 = claim_progress(&cfg, &tmp, &claim, t0);
        assert!(
            matches!(v1, Determination::Undetermined(_)),
            "first sample (no prior snapshot) must be Undetermined, got {v1:?}"
        );

        // The holder does durable work in the worktree it still has. Nothing
        // else moves: the shared project repo stays frozen, and the run's task
        // `updated_at` values are pinned by `save_run_worktrees`.
        let repo_before = head_of(&tmp);
        git_advance_head(&live);
        assert_eq!(
            repo_before,
            head_of(&tmp),
            "fixture precondition: the shared project repo HEAD must stay frozen, \
             so the live worktree's commit is the ONLY thing that can move the \
             fingerprint"
        );

        let now = t0 + window;
        let v2 = claim_progress(&cfg, &tmp, &claim, now);
        assert_eq!(
            v2,
            Determination::Known(Liveness::Progressing),
            "one recorded worktree is gone but another is being committed to; the \
             holder is demonstrably alive and an absent sibling must never erase \
             that evidence (got {v2:?})"
        );

        let (file_kept, task_kept) = survives_reap(&cfg, &tmp, &claim, now);
        assert!(
            file_kept && task_kept,
            "a live holder's claim must NEVER be reaped (file kept={file_kept}, \
             task kept={task_kept})"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// (C3) SAFETY SIDE — takes precedence over (C1). The run records a worktree
    /// that EXISTS but whose git state cannot be read. That is a genuine failure
    /// to observe, not an observation, so it must stay `Undetermined` and the
    /// claim must survive.
    ///
    /// This is the half of the split that absence must NOT swallow: "not there"
    /// is determinate, "there but unaskable" is not. Both facts are observed
    /// in-test rather than assumed — the directory really exists, and
    /// `git_head_signal` really refuses on it — because if the fixture silently
    /// became an absent path (or a readable repo) this test would go on passing
    /// while guarding nothing.
    #[test]
    fn claim_with_existing_but_unreadable_run_worktree_is_not_reaped() {
        let tmp = make_tmp_dir("claim-unreadable-live");
        let wt = tmp.join("wt-present-but-unaskable");
        make_unreadable_worktree(&wt);
        assert!(
            wt.is_dir(),
            "fixture precondition: the recorded worktree must EXIST, or this \
             test degenerates into the absent case it exists to distinguish"
        );
        assert!(
            matches!(
                progress::git_head_signal(&wt),
                Determination::Undetermined(_)
            ),
            "fixture precondition: git must genuinely refuse on this directory; \
             a plain directory would answer with the enclosing repo's HEAD and \
             this test would guard nothing"
        );

        let (cfg, claim, home) = progress_fixture(&tmp, "runUnaskable", Some(&wt));
        save_run_worktrees(&cfg, &tmp, "runUnaskable", &[("t-a", &wt)]);
        let _h = pin_home(&home);
        let window = progress::window_secs(progress::DEFAULT_WINDOW_SECS);
        let t0 = 10_000i64;

        let repo_head = head_of(&tmp);
        let _ = claim_progress(&cfg, &tmp, &claim, t0);
        let now = t0 + window;
        let v2 = claim_progress(&cfg, &tmp, &claim, now);
        assert_eq!(
            repo_head,
            head_of(&tmp),
            "fixture precondition: nothing may commit during this test"
        );
        assert!(
            matches!(v2, Determination::Undetermined(_)),
            "the worktree is THERE but git cannot be asked about it; that is a \
             failure to observe, and folding it in with absence would hand reap \
             authority to a failed read (got {v2:?})"
        );

        let (file_kept, task_kept) = survives_reap(&cfg, &tmp, &claim, now);
        assert!(
            file_kept && task_kept,
            "a claim whose run-scoped head signal could not be READ must be KEPT, \
             never force-stolen (file kept={file_kept}, task kept={task_kept})"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }
}
