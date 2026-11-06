//! Pre-checkout merge refusals of a task branch, moved verbatim out of
//! `worktree.rs` (file-growth ratchet): the recorded creation base must be on
//! the default branch, and every landed commit must carry the task trailer.

use anyhow::{bail, Context, Result};
use std::path::Path;

use super::{creation_base_key, git, git_exit_code, git_try, task_identity_key, TASK_TRAILER};

/// Refuse to integrate `branch` unless EVERY commit the merge would newly land
/// on `default_branch` (`<base>..<tip>` minus what `default_branch` already
/// has) carries a `Condukt-Task: <run>/<task>` trailer naming this task, and
/// only that task.
///
/// Parallel sessions share one git identity, so authorship cannot tell a
/// task's own commits from a foreign session's commit made on the same branch;
/// the trailer is the only per-task mark. The expected identity is the
/// `conduktTask` record written at create time; a caller-supplied
/// `--run/--task` must agree with it, and stands in for it only when no
/// record exists. Every non-determination refuses: no identity, a half-given
/// `--run/--task`, a disagreeing pair, an unreadable log, a malformed record.
///
/// Merge commits are not exempt. condukt itself never commits on a task
/// branch — [`merge`] and [`resolve_merge`] create their merge commit on
/// `default_branch` — so any merge commit in the range was made by someone in
/// the worktree (e.g. `git merge main` to resolve a conflict) and must carry
/// the trailer like any other commit. Commits merged in from `default_branch`
/// are already on it and so are outside the range.
///
/// An empty range lands nothing, so there is nothing that could be foreign and
/// the check passes; the returned tip is what the caller must merge (pinned,
/// so a commit arriving after the check is not merged unchecked).
pub(super) fn ensure_commits_carry_task_trailer(
    repo: &Path,
    branch: &str,
    default_branch: &str,
    base: &str,
    cli_task: Option<(&str, &str)>,
) -> Result<String> {
    let short = branch.strip_prefix("refs/heads/").unwrap_or(branch);
    let key = task_identity_key(short);
    // `--local`: only the repository's own record counts, never a value in
    // the global/system gitconfig (backlog 50158d9d, twin of the base read).
    let (found, value, err) = git_try(repo, &["config", "--local", "--get", &key])?;
    let recorded = value.trim();
    let recorded = (found && !recorded.is_empty()).then(|| recorded.to_string());
    let cli = cli_task.map(|(r, t)| format!("{r}/{t}"));
    let identity = match (recorded, cli) {
        (Some(rec), Some(cli)) if rec != cli => bail!(
            "merge-task-mismatch: '{branch}' is recorded as task '{rec}' ({key}) but the \
             merge was asked for task '{cli}'; refusing to merge ('{default_branch}' untouched)"
        ),
        (Some(rec), _) => rec,
        (None, Some(cli)) => cli,
        (None, None) => bail!(
            "merge-task-undetermined: '{branch}' has no recorded task identity ({key}{}) and \
             no --run/--task was given; cannot tell which commits belong to the task, \
             refusing to merge ('{default_branch}' untouched)",
            if err.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", err.trim())
            }
        ),
    };

    let tip = git(
        repo,
        &["rev-parse", "--verify", &format!("{branch}^{{commit}}")],
    )
    .with_context(|| format!("could not resolve '{branch}' to a commit"))?
    .trim()
    .to_string();
    let fmt = format!(
        "--format=%H%x1f%(trailers:key={TASK_TRAILER},valueonly,unfold,separator=%x1f)%x1e"
    );
    let log =
        git(repo, &["log", &fmt, &tip, "--not", base, default_branch]).with_context(|| {
            format!(
                "merge-task-undetermined: could not list the commits of '{branch}' not on \
             '{default_branch}'; refusing to merge ('{default_branch}' untouched)"
            )
        })?;
    let mut offending = Vec::new();
    for rec in log.split('\x1e') {
        let rec = rec.trim_matches(|c| c == '\n' || c == '\r');
        if rec.is_empty() {
            continue;
        }
        let mut fields = rec.split('\x1f');
        let sha = fields.next().unwrap_or("").trim();
        if sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!(
                "merge-task-undetermined: unparseable `git log` record {rec:?} for '{branch}'; \
                 refusing to merge ('{default_branch}' untouched)"
            );
        }
        let values: Vec<&str> = fields.map(str::trim).filter(|v| !v.is_empty()).collect();
        if values.is_empty() || values.iter().any(|v| *v != identity) {
            let got = if values.is_empty() {
                "no trailer".to_string()
            } else {
                values.join(", ")
            };
            offending.push(format!("{sha} ({got})"));
        }
    }
    if !offending.is_empty() {
        bail!(
            "merge-task-trailer-missing: {} commit(s) on '{branch}' do not carry \
             `{TASK_TRAILER}: {identity}` and may not belong to this task (a parallel session \
             shares the git identity); refusing to merge ('{default_branch}' untouched):\n  {}",
            offending.len(),
            offending.join("\n  ")
        );
    }
    Ok(tip)
}

/// Refuse to integrate `branch` into `default_branch` unless the branch's
/// recorded creation base is already an ancestor of `default_branch`
/// (backlog 51595c41). Returns the verified base.
///
/// `create` cuts the task branch from whatever HEAD the primary tree is on;
/// `merge` always integrates into `default_branch`. A run started on a feature
/// branch would therefore carry the feature's whole unreviewed history into
/// `default_branch` with nothing saying so. Every non-determination — no record
/// (e.g. a branch cut by an older binary or by hand), an unreadable config, a
/// record that does not name a commit, a failed ancestry probe — refuses too:
/// "could not tell" is not "the base is on the default branch".
pub(super) fn ensure_base_on_default_branch(
    repo: &Path,
    branch: &str,
    default_branch: &str,
) -> Result<String> {
    const HOW: &str = "condukt integrates a task branch only into the default branch and only \
         when the branch was cut from it; review `git log <default>..<branch>` and merge by \
         hand if that history is meant to land, or re-run the task from the default branch";
    let short = branch.strip_prefix("refs/heads/").unwrap_or(branch);
    let key = creation_base_key(short);
    // `--local` (backlog 50158d9d): the record is what `worktree create`
    // wrote into the repository's config. A `conduktBase` in the user's
    // global or system gitconfig is not that record and must not be honoured.
    let (found, value, err) = git_try(repo, &["config", "--local", "--get", &key])?;
    let base = value.trim();
    if !found || base.is_empty() {
        bail!(
            "merge-base-unrecorded: '{branch}' has no recorded creation base ({key}{}); \
             cannot determine whether it was cut from '{default_branch}', refusing to merge \
             ('{default_branch}' untouched). {HOW}",
            if err.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", err.trim())
            }
        );
    }
    let (resolves, _, _) = git_try(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{base}^{{commit}}"),
        ],
    )?;
    if !resolves {
        bail!(
            "merge-base-unrecorded: the recorded creation base of '{branch}' ({key} = {base}) \
             does not resolve to a commit; refusing to merge ('{default_branch}' untouched). {HOW}"
        );
    }
    // `--is-ancestor` exits 0 for yes, 1 for no; any other code (or a timeout,
    // surfaced as `Err` by `?`) is undetermined and refuses as well.
    match git_exit_code(repo, &["merge-base", "--is-ancestor", base, default_branch])? {
        0 => Ok(base.to_string()),
        1 => bail!(
            "merge-base-not-on-default-branch: '{branch}' was cut from {base}, which is not \
             on '{default_branch}'; merging it would carry that commit's history into \
             '{default_branch}' silently, refusing ('{default_branch}' untouched). {HOW}"
        ),
        code => bail!(
            "merge-base-undetermined: `git merge-base --is-ancestor {base} {default_branch}` \
             exited {code}; cannot determine whether '{branch}' was cut from \
             '{default_branch}', refusing to merge ('{default_branch}' untouched). {HOW}"
        ),
    }
}
