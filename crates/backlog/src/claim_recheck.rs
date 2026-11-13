//! `next --claim` with the stored repro re-run before the ticket is handed out
//! (backlog 78cee053). Split out of `main.rs` to keep it under its line cap.
//! A row's project identity: in a repo store, the store's repo (user ruling
//! 2026-11-14 「本質的というのは問題の質は通常パスでゆらぐものではない」 —
//! the `project` label is only where the row was filed); in a pinned
//! multi-project store, the label.
//! Where the repro is re-run (user ruling 2026-11-07, "Cwd if same project"):
//! the cwd's checkout (its git toplevel) when it belongs to that project —
//! the same `store::canonical_project_id`, so a linked worktree matches its
//! main checkout — otherwise that project's root. When a candidate is
//! provably another repo's, its project identity is undetermined, or its
//! re-run location is not a usable git checkout, that candidate is SKIPPED
//! (never auto-cancelled) and the next candidate
//! is considered. The claim is REFUSED (non-zero) when no claimable candidate
//! remains and at least one candidate was skipped due to undetermined re-run
//! location (others may have been skipped because their fix landed or they were
//! auto-cancelled). See [`rerun_dir`].

use crate::store::row_identity;
use crate::{claim_ledger, evidence, live_lease_view, store, task};
use anyhow::Result;
use harness_core::verdict::Determination;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// How many times `next --claim` re-checks when the locked pick is not the
/// candidate whose repro it just re-ran (backlog 78cee053). Exhausting it is a
/// non-zero refusal, never a claim of an un-rechecked ticket.
const RECHECK_ATTEMPTS: usize = 3;

/// The directory the candidate's stored repro is re-run in.
///
/// `store_root` is the repo holding the store (`StoreLocation::project_root`):
///
///   * **Repo store** (`Some`): the STORE'S repo is the identity of every row
///     in it (user ruling 2026-11-14:
///     「本質的というのは問題の質は通常パスでゆらぐものではない」; and
///     `config::StoreLocation::Repo`: a repo-local store holds that repo's
///     tasks "whichever checkout wrote them"). The row's `project` label is
///     only where the row was FILED, so a label naming another machine's path
///     (absent here) does not make the row foreign. See
///     [`repo_store_rerun_dir`] for the only cases the label still matters.
///   * **Pinned (multi-project) store** (`None`): the label is the only
///     separator, so it stays the identity ([`label_rerun_dir`]).
///
/// `Err` means the caller skips this candidate (never auto-cancels it) and
/// considers the next one. When no claimable candidate remains and at least
/// one was skipped this way, the claim is REFUSED instead of reporting an
/// empty queue (CLAUDE.md §3: fail-closed).
pub(crate) fn rerun_dir(
    cand: &task::Task,
    store_root: Option<&Path>,
) -> std::result::Result<PathBuf, String> {
    match store_root {
        Some(root) => repo_store_rerun_dir(cand, root),
        None => label_rerun_dir(cand),
    }
}

/// [`rerun_dir`] for a repo store: the store's repo is the row's identity.
///
/// The label can only DISPROVE that, and only when it is an absolute path
/// that exists on this machine: one that resolves to a DIFFERENT repo is
/// `Err` (foreign row), and one whose existence or identity cannot be
/// determined here is `Err` (undeterminable — never read as "same repo").
/// An absent label (filed on another machine) or a non-path label is no
/// evidence against the store, so the row is this repo's.
///
/// Re-run location (user ruling 2026-11-07, "Cwd if same project"): the cwd's
/// checkout when it belongs to the store's repo, else the store's repo root.
fn repo_store_rerun_dir(
    cand: &task::Task,
    store_root: &Path,
) -> std::result::Result<PathBuf, String> {
    match row_identity::row_repo(&cand.project, store_root) {
        row_identity::RowRepo::This => {}
        row_identity::RowRepo::Other(why) | row_identity::RowRepo::Undetermined(why) => {
            return Err(why);
        }
    }
    let store_id = match store::canonical_project_id(store_root) {
        Determination::Known(p) => p,
        Determination::Undetermined(why) => {
            return Err(format!(
                "the project identity of this store's repo {} cannot be determined: {why}",
                store_root.display()
            ));
        }
    };
    if let Some(top) = cwd_checkout_of(&store_id)? {
        return Ok(top);
    }
    checkout_root(store_root, "this store's repo root")
}

/// The cwd's checkout (its git toplevel) when the cwd belongs to project `id`;
/// `Ok(None)` when it belongs to another project or its toplevel is not
/// resolvable by git; `Err` when the cwd or its identity cannot be read.
fn cwd_checkout_of(id: &Path) -> std::result::Result<Option<PathBuf>, String> {
    let cwd = std::env::current_dir().map_err(|e| {
        format!(
            "the cwd cannot be read, so whether it belongs to the ticket's project cannot be \
             determined: {e}"
        )
    })?;
    let cwd_id = match store::canonical_project_id(&cwd) {
        Determination::Known(p) => p,
        Determination::Undetermined(why) => {
            return Err(format!(
                "the project identity of the cwd {} cannot be determined: {why}",
                cwd.display()
            ));
        }
    };
    if cwd_id == id {
        if let Ok(top) = evidence::repo_toplevel(&cwd) {
            if let Ok(top) = top.canonicalize() {
                return Ok(Some(top));
            }
        }
    }
    Ok(None)
}

/// [`rerun_dir`] for a pinned (multi-project) store, where the row's label is
/// its identity (user ruling 2026-11-07, "Cwd if same project"):
///   1. the cwd's checkout (its git toplevel) when its
///      `store::canonical_project_id` equals that of the ticket's `project`
///      field (a linked worktree maps to its main checkout, so a worktree of
///      the ticket's project re-runs in the worktree);
///   2. otherwise (the cwd belongs to another project, is not inside a repo,
///      or its toplevel is not resolvable by git) the ticket's project root,
///      validated by [`project_root`].
///
/// `Err` (the caller skips this candidate and considers the next one) when the
/// ticket's project field is unresolved or empty, when EITHER project identity
/// is `Undetermined` (or the cwd itself cannot be read) — an undetermined
/// identity is never read as "same project" nor as "different project" — or when
/// the fallback project root is not a usable git checkout. When no claimable
/// candidate remains and at least one candidate was skipped as undetermined, the
/// claim is REFUSED instead of returning empty queue (CLAUDE.md §3: fail-closed).
fn label_rerun_dir(cand: &task::Task) -> std::result::Result<PathBuf, String> {
    if cand.project_unresolved {
        return Err(format!(
            "its project root was recorded as unresolved ({:?}), so its project identity cannot \
             be determined",
            cand.project
        ));
    }
    if cand.project.trim().is_empty() {
        return Err(
            "it records no project root, so its project identity cannot be determined".to_string(),
        );
    }
    let ticket_id = match store::canonical_project_id(Path::new(&cand.project)) {
        Determination::Known(p) => p,
        Determination::Undetermined(why) => {
            return Err(format!(
                "the project identity of its project root {:?} cannot be determined: {why}",
                cand.project
            ));
        }
    };
    if let Some(top) = cwd_checkout_of(&ticket_id)? {
        return Ok(top);
    }
    project_root(cand)
}

/// The ticket's project root (the row's `project` field) as a re-run
/// location. `Err` (the caller skips this candidate) when the root is
/// unresolved, empty, missing, or not itself the top of a git checkout —
/// re-running against any other tree would hand out a ticket on
/// someone else's evidence.
fn project_root(cand: &task::Task) -> std::result::Result<PathBuf, String> {
    if cand.project_unresolved {
        return Err(format!(
            "its project root was recorded as unresolved ({:?}), so there is no checkout to \
             re-run it in",
            cand.project
        ));
    }
    if cand.project.trim().is_empty() {
        return Err("it records no project root to re-run it in".to_string());
    }
    checkout_root(Path::new(&cand.project), "its project root")
}

/// `path` as a re-run location. `Err` when it is missing, not a directory, or
/// not itself the top of a git checkout — re-running against any other tree
/// would hand out a ticket on someone else's evidence. `what` names `path` in
/// the reason.
fn checkout_root(path: &Path, what: &str) -> std::result::Result<PathBuf, String> {
    let root = path
        .canonicalize()
        .map_err(|e| format!("{what} {:?} cannot be resolved: {e}", path.display()))?;
    if !root.is_dir() {
        return Err(format!("{what} {} is not a directory", root.display()));
    }
    let top = evidence::repo_toplevel(&root)
        .map_err(|e| format!("{what} {} is not a git checkout: {e}", root.display()))?;
    let top = top.canonicalize().map_err(|e| {
        format!(
            "the git toplevel {} of {what} cannot be resolved: {e}",
            top.display()
        )
    })?;
    if top != root {
        return Err(format!(
            "{what} {} is not a git checkout (it lies inside the checkout {})",
            root.display(),
            top.display()
        ));
    }
    Ok(root)
}

/// Whether the fix commit of `cand` (a commit reachable from HEAD whose subject
/// contains `(backlog <id>)`) has already landed (backlog a04b2b87). HEAD is
/// the one of the location [`rerun_dir`] picks (with the same `store_root`),
/// NOT the process cwd, so a `next` run from a non-git cwd still looks at the
/// ticket's project root. `Err` when that location or git's answer cannot be
/// determined; the caller refuses rather than hand out a possibly-fixed ticket.
pub(crate) fn fix_landed(
    cand: &task::Task,
    store_root: Option<&Path>,
) -> std::result::Result<bool, String> {
    let root = rerun_dir(cand, store_root)?;
    Ok(evidence::fixed_task_ids(&root)?.contains(&cand.id))
}

/// Plain `next` (no claim): like `store::next_excluding`, but a candidate whose
/// fix already landed is skipped with a stderr notice and the next candidate
/// is considered.
///
/// `store_root` is the repo holding the store (`StoreLocation::project_root`).
/// With a repo store, the landed-fix set is `evidence::fixed_task_ids` of that
/// repo's HEAD, computed ONCE before the loop; the ticket's `project` label is
/// never consulted (a label naming another machine's path must not refuse the
/// queue). If that set cannot be determined, the command is REFUSED. With a
/// pinned (multi-project) store (`None`), each candidate is checked by
/// [`fix_landed`] at the location [`rerun_dir`] picks, refusing when that is
/// undetermined. Never a silent hand-out of a possibly-fixed ticket.
pub(crate) fn next_unfixed(
    tasks_path: &Path,
    tag: Option<&str>,
    project: Option<&str>,
    leased: &HashSet<String>,
    store_root: Option<&Path>,
) -> Result<Option<task::Task>> {
    let repo_fixed = match store_root {
        Some(root) => Some(evidence::fixed_task_ids(root).map_err(|why| {
            anyhow::anyhow!(
                "backlog next REFUSED (this is NOT \"no pending tasks\"): which fixes already \
                 landed in {} could not be determined: {why}",
                root.display()
            )
        })?),
        None => None,
    };
    let mut excluded: HashSet<String> = leased.clone();
    loop {
        let Some(cand) = store::next_excluding(tasks_path, tag, project, &excluded)? else {
            return Ok(None);
        };
        let landed = match &repo_fixed {
            Some(ids) => Ok(ids.contains(&cand.id)),
            None => fix_landed(&cand, None),
        };
        match landed {
            Ok(false) => return Ok(Some(cand)),
            Ok(true) => {
                eprintln!(
                    "backlog next: NOT handing out {}: a commit `(backlog {})` is already an \
                     ancestor of HEAD, so it is likely fixed; verify and close it. Considering \
                     the next candidate.",
                    cand.id, cand.id
                );
                excluded.insert(cand.id.clone());
            }
            Err(why) => {
                return Err(anyhow::anyhow!(
                    "backlog next REFUSED (this is NOT \"no pending tasks\"): whether the fix \
                     of candidate {} already landed could not be determined: {why}",
                    cand.id
                ));
            }
        }
    }
}

/// `next --claim` with the stored repro RE-RUN before the ticket is handed out
/// (backlog 78cee053, audit B item 6: a RED ticket is often already fixed).
///
/// Each round: peek the top candidate (a pure read, NO lock held); if its fix
/// commit already landed ([`fix_landed`]) it joins `skip` with a stderr notice
/// (undetermined: REFUSED); otherwise re-run its
/// stored repro at HEAD of the location [`rerun_dir`] picks for `store_root`
/// (repo store: the store's repo is the row's identity, user ruling
/// 2026-11-14; pinned store: the row's label. The cwd checkout when it belongs
/// to that project, else that project's root; a row that is provably another
/// repo's, an undetermined identity or an unusable root is SKIPPED with a
/// notice, never auto-cancelled, and the claim moves to the next candidate)
/// via `evidence::run_repro`
/// (can be a long `cargo test`, so it runs outside the ledger lock and the
/// tasks-file lock), classified by `evidence::run_repro_classified`, then:
///   - `not-reproduced` / `not-demonstrated` (user ruling 2026-11-13
///     「保留はありえない実証されないならclose,実証されたらopen」): not handed
///     out and AUTO-CANCELLED ([`auto_cancel`]: discard closure recording the
///     verdict, cmd, rev, exit code and output tail; the mirrored issue is
///     closed like `backlog cancel`). A stderr notice names it and the next
///     candidate is considered. When the store lies in the PRIMARY working
///     tree (store writes refused there, backlog 1e6f00ae) nothing is written
///     and the claim is REFUSED, naming the candidate and the observation.
///   - `undeterminable` (infrastructure: timeout, signal, build failure, ...):
///     REFUSED (user ruling 2026-11-07, "Refuse the claim"): non-zero, the
///     runner's detail in the error, nothing claimed, the row untouched.
///   - `reproduced` (RED): claimed through `claim_ledger::claim_pinned`, which
///     reserves ONLY this candidate. If the locked pick differs, nothing was
///     reserved and the round repeats, at most [`RECHECK_ATTEMPTS`] times.
///
/// `Ok(None)` only when no eligible candidate remains AND none was skipped as
/// undetermined (truly empty queue). When no claimable candidate remains and at
/// least one candidate was skipped due to undetermined re-run location, returns
/// Err (REFUSED) instead, because the queue is not judged empty — those
/// candidates are pending but unverifiable.
pub(crate) fn claim_rechecked(
    tasks_path: &Path,
    tag: Option<&str>,
    project: Option<&str>,
    identity: &str,
    session_id: Option<&str>,
    store_root: Option<&Path>,
) -> Result<Option<task::Task>> {
    const REFUSED: &str = "backlog next --claim REFUSED to claim (this is NOT \"no pending \
                           tasks\" — nothing was claimed and the queue was not judged empty)";
    let mut skip: HashSet<String> = HashSet::new();
    let mut undetermined: Vec<(String, String, String)> = Vec::new();
    let mut moved_rounds = 0usize;
    loop {
        let mut excluded: HashSet<String> = live_lease_view(project, "next --claim")?
            .into_keys()
            .collect();
        excluded.extend(skip.iter().cloned());
        let Some(cand) = store::next_excluding(tasks_path, tag, project, &excluded)? else {
            if undetermined.is_empty() {
                return Ok(None);
            }
            let mut msg = format!(
                "{REFUSED}: no claimable candidate remains and the re-run of these skipped candidates could not be determined (they are left pending):"
            );
            for (id, cmd, why) in undetermined {
                msg.push_str(&format!("\n  - {id} (cmd: {cmd}): {why}"));
            }
            return Err(anyhow::anyhow!(msg));
        };
        // `pick_next` only returns rows with a reproduced repro, so a missing
        // record here is a broken invariant, refused rather than claimed.
        let Some(stored) = cand.repro.as_ref() else {
            return Err(anyhow::anyhow!(
                "{REFUSED}: candidate {} carries no stored repro to re-run",
                cand.id
            ));
        };
        let root = match rerun_dir(&cand, store_root) {
            Ok(r) => r,
            Err(why) => {
                eprintln!(
                    "backlog next --claim: NOT handing out {}: the re-run of its stored repro \
                     could not be determined (cmd: {}): {why}. It is left pending (not \
                     cancelled). Considering the next candidate.",
                    cand.id, stored.cmd
                );
                undetermined.push((cand.id.clone(), stored.cmd.clone(), why));
                skip.insert(cand.id.clone());
                continue;
            }
        };
        // Landed-fix check at HEAD of the SAME location the repro re-runs in
        // (what `fix_landed(&cand, store_root)` would resolve again).
        match evidence::fixed_task_ids(&root).map(|ids| ids.contains(&cand.id)) {
            Ok(false) => {}
            Ok(true) => {
                eprintln!(
                    "backlog next --claim: NOT handing out {}: a commit `(backlog {})` is \
                     already an ancestor of HEAD, so it is likely fixed; verify and close it. \
                     Considering the next candidate.",
                    cand.id, cand.id
                );
                skip.insert(cand.id.clone());
                continue;
            }
            Err(why) => {
                return Err(anyhow::anyhow!(
                    "{REFUSED}: whether the fix of candidate {} already landed could not be \
                     determined: {why}",
                    cand.id
                ));
            }
        }
        let classified = evidence::run_repro_classified(&root, &stored.cmd, store::now_unix());
        let rerun = &classified.repro;
        let rev = rerun.rev.as_deref().unwrap_or("<unresolved>");
        match classified.verdict {
            evidence::RecheckVerdict::Reproduced => {}
            // User ruling 2026-11-13: not demonstrated -> close. The row is
            // auto-cancelled (discard closure, issue closed) and the claim
            // moves on to the next candidate in the same invocation.
            evidence::RecheckVerdict::NotReproduced | evidence::RecheckVerdict::NotDemonstrated => {
                if let Err(why) =
                    harness_core::primary_tree::refuse_if_primary(tasks_path, "auto-cancel")
                {
                    return Err(anyhow::anyhow!(
                        "{REFUSED}: candidate {}'s stored repro is {} (cmd: {}, rev: {rev}): {}. \
                         It must be auto-cancelled (user ruling 2026-11-13), but this store \
                         cannot be written here: {why}. Run `backlog next --claim` or \
                         `backlog recheck --id {}` from a linked worktree.",
                        cand.id,
                        classified.verdict.label(),
                        rerun.cmd,
                        rerun.detail,
                        cand.id
                    ));
                }
                auto_cancel(tasks_path, &cand.id, &classified, "next --claim").map_err(|e| {
                    anyhow::anyhow!(
                        "{REFUSED}: candidate {}'s stored repro is {} but auto-cancelling it \
                         failed: {e}",
                        cand.id,
                        classified.verdict.label()
                    )
                })?;
                eprintln!(
                    "backlog next --claim: AUTO-CANCELLED {} (user ruling 2026-11-13: not \
                     demonstrated -> close): its stored repro is {} (cmd: {}, rev: {rev}): {}. \
                     Commit the store change. Considering the next candidate.",
                    cand.id,
                    classified.verdict.label(),
                    rerun.cmd,
                    rerun.detail
                );
                skip.insert(cand.id.clone());
                continue;
            }
            evidence::RecheckVerdict::Undeterminable => {
                return Err(anyhow::anyhow!(
                    "{REFUSED}: the re-run of candidate {}'s stored repro could not be \
                     determined (infrastructure; NOT auto-cancelled; outcome {}, cmd: {}, rev: \
                     {rev}): {}",
                    cand.id,
                    classified.verdict.label(),
                    rerun.cmd,
                    rerun.detail
                ));
            }
        }
        let claimed = claim_ledger::claim_pinned(
            tasks_path, tag, project, identity, session_id, None, &skip, &cand.id,
        )?;
        match claimed {
            Determination::Known(claim_ledger::PinnedClaim::Claimed(t)) => return Ok(Some(*t)),
            Determination::Known(claim_ledger::PinnedClaim::Moved(picked)) => {
                moved_rounds += 1;
                if moved_rounds >= RECHECK_ATTEMPTS {
                    return Err(anyhow::anyhow!(
                        "{REFUSED}: the locked pick kept differing from the re-checked candidate \
                         ({moved_rounds} attempts; last re-checked {}, locked pick {}); a ticket \
                         whose repro was not re-run is never handed out",
                        cand.id,
                        picked.as_deref().unwrap_or("<none eligible>")
                    ));
                }
            }
            // REFUSED, not empty. Non-zero exit, reason on stderr, nothing on
            // stdout — the identical shape `guard_store_divergence` uses, and
            // the shape `autoflow::backlog::find_open` reads as
            // `Determination::Undetermined` rather than "no work".
            Determination::Undetermined(why) => {
                return Err(anyhow::anyhow!("{REFUSED}: {why}"));
            }
        }
    }
}

/// The user ruling every auto-cancel quotes (2026-11-13).
pub(crate) const NO_DEFERRAL_RULING: &str =
    "「保留はありえない実証されないならclose,実証されたらopen」";

/// The discard reason recorded by an auto-cancel: the ruling, which command
/// observed it, and the observation verbatim (verdict, rev, cmd, exit code
/// and output tail as carried by the repro detail).
pub(crate) fn auto_cancel_reason(by: &str, c: &evidence::ClassifiedRepro) -> String {
    let exit = match c.exit {
        Some(e) => format!("exit {e}"),
        None => "no completed run".to_string(),
    };
    format!(
        "auto-cancel by `backlog {by}` (user ruling 2026-11-13 {NO_DEFERRAL_RULING}): the stored \
         repro was re-run and did NOT demonstrate the defect. verdict: {}; {exit}; cmd: {}; rev: \
         {}; observed: {}",
        c.verdict.label(),
        c.repro.cmd,
        c.repro.rev.as_deref().unwrap_or("<unresolved>"),
        c.repro.detail
    )
}

/// Auto-cancel `id` with `cancel` semantics (discard closure via
/// `store::mark_cancelled`) and close its mirrored issue exactly like
/// `backlog cancel` (`mirror_close_for`). The caller has already checked the
/// store is writable (not the primary working tree).
pub(crate) fn auto_cancel(
    tasks_path: &Path,
    id: &str,
    c: &evidence::ClassifiedRepro,
    by: &str,
) -> Result<()> {
    debug_assert!(c.verdict.closes());
    store::mark_cancelled(tasks_path, id, &auto_cancel_reason(by, c))?;
    crate::mirror_close_for(tasks_path, id);
    Ok(())
}
