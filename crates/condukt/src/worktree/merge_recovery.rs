//! Rollback and truthful reporting of a `git merge` that failed or was killed
//! on [`GIT_TIMEOUT`], moved out of `worktree.rs` (file-growth ratchet) with
//! the fix for GH #1638 / #1639: the no-commit trial abort
//! ([`abort_trial_merge`]) and the timed-out-merge rollback shared by every
//! merge path that commits onto the default branch
//! ([`run_merge_recovering_timeout`]).

use anyhow::{bail, Context, Result};
use std::path::Path;

use super::{git, git_exit_code, git_try, run_git_bounded, GitOutput, GIT_TIMEOUT};

/// Abort an in-progress trial merge and say truthfully what state it left.
/// `Ok(())` when no merge is in progress afterwards: either `git merge --abort`
/// succeeded, or it failed AND a probe shows MERGE_HEAD absent (git refused the
/// trial before creating merge state, so there is nothing to abort and the tree
/// is untouched). A merge still in progress, or a probe that cannot be run or
/// returns an unexpected status, is `Err(detail)` (cannot-determine stays
/// fail-closed: the repo may be left mid-merge). A failed abort with an
/// `index.lock` present (or unprobeable) is also `Err` naming the lock (GH
/// #1638: a killed git leaves the lock, `merge --abort` then fails on it, and
/// the absent MERGE_HEAD used to read as "aborted" while every later git
/// command in that tree fails). The lock is NOT removed: condukt cannot prove
/// no live git holds it.
pub(super) fn abort_trial_merge(repo: &Path) -> std::result::Result<(), String> {
    let detail = match git_try(repo, &["merge", "--abort"]) {
        Ok((true, _, _)) => return Ok(()),
        Ok((false, out, err)) => format!("{} {}", out.trim(), err.trim()).trim().to_string(),
        Err(e) => format!("{e:#}"),
    };
    let git_dir = git(repo, &["rev-parse", "--absolute-git-dir"]).map_err(|e| {
        format!("{detail}; could not locate the git dir to probe index.lock: {e:#}")
    })?;
    let lock = Path::new(&git_dir).join("index.lock");
    match lock.try_exists() {
        Ok(false) => {}
        Ok(true) => {
            return Err(format!(
                "{detail}; {} is present (a killed git's or a live git's, so condukt did NOT \
                 remove it; every git command there fails until it is removed once no git \
                 process runs there)",
                lock.display()
            ))
        }
        Err(e) => return Err(format!("{detail}; could not probe {}: {e}", lock.display())),
    }
    // `rev-parse -q --verify`: exit 0 = MERGE_HEAD present, 1 = absent.
    match git_exit_code(repo, &["rev-parse", "-q", "--verify", "MERGE_HEAD"]) {
        Ok(1) => Ok(()),
        Ok(0) => Err(format!("{detail}; MERGE_HEAD is still present")),
        Ok(c) => Err(format!(
            "{detail}; MERGE_HEAD probe returned unexpected status {c}"
        )),
        Err(e) => Err(format!("{detail}; MERGE_HEAD probe failed: {e:#}")),
    }
}

/// Self-contained sentence describing the trial-merge state, for a fail-closed
/// error.
pub(super) fn abort_state_phrase(r: &std::result::Result<(), String>) -> String {
    match r {
        Ok(()) => "The trial merge was aborted (no merge in progress).".to_string(),
        Err(d) => format!(
            "The trial merge could NOT be aborted ({d}); the repository may be left \
             mid-merge - run `git merge --abort`."
        ),
    }
}

/// Run a `git merge` (`merge_args`) in `repo`, bounded by [`GIT_TIMEOUT`].
/// When it did NOT time out, the raw output is returned for the caller to judge
/// its status. When it timed out, the killed merge may have staged its result
/// with no MERGE_HEAD (backlog 8ada7ad9) or left a stale `index.lock`
/// (GH #1638), so it is rolled back with [`recover_timed_out_merge`] and this is
/// ALWAYS an `Err` that says "timed out" plus either the VERIFIED rollback or,
/// when the rollback failed or could not be verified, that the tree may be
/// HALF-APPLIED with the manual recovery (and the `index.lock` detail, if any).
/// `what` names the operation ("merge of X into Y"); `target` is the branch
/// checked out in `repo`. Shared by every merge path that commits onto the
/// default branch ([`merge`], [`merge_carrying_store_diff`], [`resolve_merge`]).
pub(super) fn run_merge_recovering_timeout(
    repo: &Path,
    merge_args: &[&str],
    head_before: &str,
    target: &str,
    what: &str,
) -> Result<GitOutput> {
    let out = run_git_bounded(repo, merge_args).with_context(|| format!("{what} failed"))?;
    if !out.timed_out {
        return Ok(out);
    }
    let (r, t) = (repo.display(), GIT_TIMEOUT);
    let timed_out = format!("git {merge_args:?} in {r} timed out after {t:?} and was killed");
    match recover_timed_out_merge(repo, head_before) {
        Ok(how) => bail!(
            "{what} failed: {timed_out}. The half-applied merge was rolled back with `{how}` \
             (index clean, HEAD unchanged at {head_before}); the merge was NOT performed and \
             can be retried."
        ),
        Err(d) => bail!(
            "{what} failed: {timed_out}. {target} in {r} may be left HALF-APPLIED (the merge \
             result staged without a merge commit) and the automatic rollback FAILED: {d}. Do \
             NOT commit in that tree; recover manually there with `git merge --abort` if \
             .git/MERGE_HEAD exists, else `git reset --merge`, then confirm \
             `git diff --cached --quiet` and that HEAD is {head_before}."
        ),
    }
}

/// Roll back a `git merge` in `repo` killed on timeout (backlog 8ada7ad9), each
/// step bounded by [`GIT_TIMEOUT`]: `git merge --abort` if MERGE_HEAD exists,
/// else `git reset --merge` (the 2026-11-07 repro left the result staged with
/// NO MERGE_HEAD). `Ok(cmd)` only after the rollback is VERIFIED (HEAD ==
/// `head_before`, nothing staged, no MERGE_HEAD); anything else, including an
/// unreadable probe, is `Err(detail)`. A present `index.lock` is NOT removed
/// (condukt cannot prove it is the killed git's, not a live git's): `Err`.
pub(super) fn recover_timed_out_merge(
    repo: &Path,
    head_before: &str,
) -> std::result::Result<String, String> {
    // Exit 0 → true, exit 1 → false; any other outcome is undetermined → Err.
    let probe = |what: &str, args: &[&str]| match git_exit_code(repo, args) {
        Ok(0) => Ok(true),
        Ok(1) => Ok(false),
        Ok(c) => Err(format!("{what} probe exited {c}")),
        Err(e) => Err(format!("{what} probe failed: {e:#}")),
    };
    let merge_head = ["rev-parse", "-q", "--verify", "MERGE_HEAD"];
    let git_dir = git(repo, &["rev-parse", "--absolute-git-dir"])
        .map_err(|e| format!("could not locate the git dir: {e:#}"))?;
    let lock = Path::new(&git_dir).join("index.lock");
    let locked = lock.try_exists();
    if !matches!(locked, Ok(false)) {
        return Err(format!(
            "{} is present or unreadable ({locked:?}; the killed git's or a live git's, so condukt \
             did NOT remove it or touch the index; remove it once no git process runs there)",
            lock.display()
        ));
    }
    let cmd: &[&str] = if probe("MERGE_HEAD", &merge_head)? {
        &["merge", "--abort"]
    } else {
        &["reset", "--merge"]
    };
    let shown = format!("git {}", cmd.join(" "));
    match git_try(repo, cmd) {
        Ok((true, _, _)) => {}
        Ok((false, o, e)) => return Err(format!("`{shown}` failed: {} {}", o.trim(), e.trim())),
        Err(e) => return Err(format!("`{shown}` failed: {e:#}")),
    }
    let after = |e: String| format!("after `{shown}`, {e}");
    let head_after = git(repo, &["rev-parse", "--verify", "HEAD"])
        .map_err(|e| after(format!("could not read HEAD: {e:#}")))?;
    if head_after != head_before {
        return Err(after(format!(
            "HEAD is {head_after}, not {head_before} (committed before the kill?)"
        )));
    }
    if !probe("staged-changes", &["diff", "--cached", "--quiet"]).map_err(after)? {
        return Err(after("changes are still staged".to_string()));
    }
    if probe("MERGE_HEAD", &merge_head).map_err(after)? {
        return Err(after("MERGE_HEAD is still present".to_string()));
    }
    Ok(shown)
}
