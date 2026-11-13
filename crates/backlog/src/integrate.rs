//! Land one ledger row on the primary branch (user ruling: 「backlog addの時点で
//! 台帳も更新される。mainにいく、issueができる」).
//!
//! [`land_row_on_main`] is the production `land` that `store::add_finding`
//! runs BEFORE `gh issue create` (and once more after it, to carry the issue
//! number). It never edits the primary working tree by hand: it builds a
//! commit `C` with plumbing (parent = tip of the branch checked out in the
//! primary tree, tree = that tip's tree with only the store file replaced by
//! the tip's store plus this row), MERGES `C` into that branch from the
//! primary tree (CLAUDE.md §8: the primary tree only integrates), verifies the
//! row on the branch, and pushes the branch to `origin`.
//!
//! Every step is fail-closed (CLAUDE.md §3): a git call that cannot be run,
//! times out, or exits non-zero is an `Err`, and `add_finding` then creates no
//! issue. A merge already in progress in the primary tree is never touched;
//! only a `MERGE_HEAD` equal to our own `C` is aborted.

use anyhow::{anyhow, bail, Context, Result};
use harness_core::boundary;
use harness_core::primary_tree::{classify, TreeKind};
use harness_core::verdict::Determination;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::store;

/// Local plumbing reads/writes.
const PLUMBING_TIMEOUT: Duration = Duration::from_secs(30);
/// `merge` and `push` run the repository's hooks.
const HOOKED_TIMEOUT: Duration = Duration::from_secs(600);

/// One finished git run. `Err` from [`git`] means it did not finish (spawn
/// failure, signal, timeout, unreadable output) — never a code.
struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

fn git(
    dir: &Path,
    args: &[&str],
    timeout: Duration,
    index: Option<&Path>,
    stdin: Option<&str>,
) -> Result<Ran> {
    let mut cmd = Command::new("git");
    harness_core::git_env::scrub(&mut cmd);
    cmd.arg("-C").arg(dir).args(args);
    if let Some(i) = index {
        cmd.env("GIT_INDEX_FILE", i);
    }
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let shown = format!("git -C {} {}", dir.display(), args.join(" "));
    let out = match boundary::run_with_timeout_and_stdin(
        &mut cmd,
        timeout,
        stdin.map(|s| s.as_bytes().to_vec()),
    ) {
        Determination::Known(o) => o,
        Determination::Undetermined(why) => {
            bail!("`{shown}` did not run to completion: {why}")
        }
    };
    let code = out.code();
    let stderr = out.stderr().to_string();
    let stdout = match out.stdout_allowing(&[code]) {
        Determination::Known(s) => s,
        Determination::Undetermined(why) => bail!("`{shown}`: {why}"),
    };
    Ok(Ran {
        code,
        stdout,
        stderr,
    })
}

/// stdout (trimmed) of a git call that must exit 0.
fn git_ok(dir: &Path, args: &[&str], index: Option<&Path>, stdin: Option<&str>) -> Result<String> {
    let r = git(dir, args, PLUMBING_TIMEOUT, index, stdin)?;
    if r.code != 0 {
        bail!(
            "`git -C {} {}` exited {}: {}",
            dir.display(),
            args.join(" "),
            r.code,
            r.stderr.trim()
        );
    }
    Ok(r.stdout.trim().to_string())
}

/// The primary worktree and the branch checked out there, from the FIRST
/// record of `git worktree list --porcelain` (git always lists the main
/// worktree first). A bare repo or a detached primary HEAD is an `Err`.
fn primary_and_branch(wt: &Path) -> Result<(PathBuf, String)> {
    let listing = git_ok(wt, &["worktree", "list", "--porcelain"], None, None)?;
    let first: Vec<&str> = listing.lines().take_while(|l| !l.is_empty()).collect();
    let dir = first
        .iter()
        .find_map(|l| l.strip_prefix("worktree "))
        .ok_or_else(|| anyhow!("`git worktree list --porcelain` named no main worktree"))?;
    if first.contains(&"bare") {
        bail!("the repository's main worktree {dir} is bare; there is no primary branch checkout to merge into");
    }
    let branch = first
        .iter()
        .find_map(|l| l.strip_prefix("branch refs/heads/"))
        .ok_or_else(|| {
            anyhow!("the primary tree {dir} has no branch checked out (detached HEAD); cannot tell which branch the row belongs on")
        })?;
    Ok((PathBuf::from(dir), branch.to_string()))
}

/// The store document at `rev:rel` parsed as rows; absent file = no rows.
fn rows_at(wt: &Path, rev: &str, rel: &str) -> Result<(String, Vec<crate::task::Task>)> {
    let listed = git_ok(wt, &["ls-tree", rev, "--", rel], None, None)?;
    if listed.is_empty() {
        return Ok((String::new(), Vec::new()));
    }
    let spec = format!("{rev}:{rel}");
    let text = git(
        wt,
        &["cat-file", "blob", &spec],
        PLUMBING_TIMEOUT,
        None,
        None,
    )?;
    if text.code != 0 {
        bail!(
            "`git cat-file blob {spec}` exited {}: {}",
            text.code,
            text.stderr.trim()
        );
    }
    let rows = store::parse_tasks_text(&text.stdout, &spec)?;
    Ok((text.stdout, rows))
}

/// Put the current worktree row `id` of the store at `store_path` onto the
/// branch checked out in the primary tree (by merge), verify it there, and
/// push that branch to `origin`.
///
/// Scope: a store in a LINKED worktree only. A store in no repository
/// (`NoRepo`, e.g. one pinned outside any checkout) has no primary branch to
/// land on and returns `Ok(())` without doing anything — `add` then behaves
/// as before for it. Every other classification (primary tree, inside a git
/// dir, undetermined) is an `Err`.
pub fn land_row_on_main(store_path: &Path, id: &str) -> Result<()> {
    match classify(store_path) {
        TreeKind::Linked => {}
        TreeKind::NoRepo => return Ok(()),
        other => bail!(
            "the store {} is not in a linked worktree ({other:?}); refusing to land row {id}",
            store_path.display()
        ),
    }
    let store_abs = store_path
        .canonicalize()
        .with_context(|| format!("cannot resolve {}", store_path.display()))?;
    let store_dir = store_abs
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent dir", store_abs.display()))?;
    let wt = PathBuf::from(git_ok(
        store_dir,
        &["rev-parse", "--show-toplevel"],
        None,
        None,
    )?)
    .canonicalize()
    .context("cannot resolve the worktree toplevel")?;
    let rel = store_abs
        .strip_prefix(&wt)
        .with_context(|| format!("{} is not under {}", store_abs.display(), wt.display()))?
        .to_str()
        .ok_or_else(|| anyhow!("non-UTF-8 store path {}", store_abs.display()))?
        .to_string();

    let (primary, branch) = primary_and_branch(&wt)?;
    let ctx = |what: &str| {
        format!(
            "{what} (branch {branch}, primary tree {})",
            primary.display()
        )
    };
    let tip_ref = format!("refs/heads/{branch}");
    let tip = git_ok(
        &wt,
        &["rev-parse", "--verify", &format!("{tip_ref}^{{commit}}")],
        None,
        None,
    )
    .with_context(|| ctx("the primary branch has no commit to merge into"))?;

    let row = store::load(&store_abs)?
        .into_iter()
        .find(|t| t.id == id)
        .ok_or_else(|| anyhow!("row {id} is not in {}", store_abs.display()))?;
    let want = store::serialize_tasks(std::slice::from_ref(&row))?;
    let (main_text, mut rows) = rows_at(&wt, &tip, &rel)?;
    match rows.iter_mut().find(|t| t.id == id) {
        Some(t) => *t = row,
        None => rows.push(row),
    }
    let new_text = store::serialize_tasks(&rows)?;

    if new_text != main_text {
        let commit = build_commit(&wt, &tip, &rel, &new_text, id)?;
        merge_into_primary(&primary, &branch, &commit)?;
        let anc = git(
            &primary,
            &["merge-base", "--is-ancestor", &commit, &tip_ref],
            PLUMBING_TIMEOUT,
            None,
            None,
        )?;
        if anc.code != 0 {
            bail!(ctx(&format!(
                "after the merge, commit {commit} is not on the branch (merge-base exit {})",
                anc.code
            )));
        }
    }
    let (_, landed) = rows_at(&primary, &tip_ref, &rel)?;
    let on_branch = landed
        .iter()
        .find(|t| t.id == id)
        .map(|t| store::serialize_tasks(std::slice::from_ref(t)))
        .transpose()?;
    if on_branch.as_deref() != Some(want.as_str()) {
        bail!(ctx(&format!(
            "row {id} on the branch is missing or differs from the worktree row after the merge"
        )));
    }

    let push = git(
        &primary,
        &["push", "origin", &format!("{tip_ref}:{tip_ref}")],
        HOOKED_TIMEOUT,
        None,
        None,
    )
    .with_context(|| ctx("push to origin did not finish; row is on the local branch only"))?;
    if push.code != 0 {
        bail!(ctx(&format!(
            "push to origin failed (exit {}); row is on the local branch but NOT on origin: {}",
            push.code,
            push.stderr.trim()
        )));
    }
    Ok(())
}

/// Commit `tip`'s tree with `rel` replaced by `text`, parent `tip`, using a
/// private temporary index (the worktree's own index is not touched).
fn build_commit(wt: &Path, tip: &str, rel: &str, text: &str, id: &str) -> Result<String> {
    let blob = git_ok(wt, &["hash-object", "-w", "--stdin"], None, Some(text))?;
    let idx_rel = git_ok(
        wt,
        &[
            "rev-parse",
            "--git-path",
            &format!("backlog-integrate.index.{}", std::process::id()),
        ],
        None,
        None,
    )?;
    let idx = wt.join(idx_rel);
    let built = (|| -> Result<String> {
        git_ok(wt, &["read-tree", tip], Some(&idx), None)?;
        git_ok(
            wt,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{blob},{rel}"),
            ],
            Some(&idx),
            None,
        )?;
        git_ok(wt, &["write-tree"], Some(&idx), None)
    })();
    // Scratch cleanup only; the verdict is `built`. A leftover temp index is
    // inert (nothing reads it) and named per pid.
    let _ = std::fs::remove_file(&idx);
    let tree = built?;
    git_ok(
        wt,
        &[
            "commit-tree",
            &tree,
            "-p",
            tip,
            "-m",
            &format!("backlog: file {id}"),
        ],
        None,
        None,
    )
}

/// `git merge --no-ff <commit>` in the primary tree, refusing to start when a
/// merge is already in progress there, and aborting only our own failed merge.
fn merge_into_primary(primary: &Path, branch: &str, commit: &str) -> Result<()> {
    let ctx = |what: String| {
        anyhow!(
            "{what} (branch {branch}, primary tree {})",
            primary.display()
        )
    };
    let git_dir = PathBuf::from(git_ok(
        primary,
        &["rev-parse", "--absolute-git-dir"],
        None,
        None,
    )?);
    let merge_head = git_dir.join("MERGE_HEAD");
    if merge_head.exists() {
        return Err(ctx(
            "a merge is already in progress in the primary tree; not touching it".into(),
        ));
    }
    let head = git_ok(primary, &["symbolic-ref", "-q", "HEAD"], None, None)?;
    if head != format!("refs/heads/{branch}") {
        return Err(ctx(format!("the primary tree's HEAD moved to {head}")));
    }
    let msg = format!("Merge backlog row into {branch}");
    let r = git(
        primary,
        &["merge", "--no-ff", "--no-edit", "-m", &msg, commit],
        HOOKED_TIMEOUT,
        None,
        None,
    )
    .map_err(|e| {
        ctx(format!(
            "`git merge` did not finish ({e:#}); the primary tree may be mid-merge and was not touched further"
        ))
    })?;
    if r.code == 0 {
        return Ok(());
    }
    // Abort ONLY a MERGE_HEAD we can read and that is our commit; an absent or
    // unreadable one is left alone (never touch what we cannot identify).
    let aborted = match std::fs::read_to_string(&merge_head) {
        Ok(s) if s.trim() == commit => {
            match git(primary, &["merge", "--abort"], PLUMBING_TIMEOUT, None, None) {
                Ok(a) if a.code == 0 => "our merge was aborted".to_string(),
                Ok(a) => format!("`git merge --abort` ALSO failed: {}", a.stderr.trim()),
                Err(e) => format!("`git merge --abort` did not finish: {e:#}"),
            }
        }
        Ok(_) => "MERGE_HEAD is not ours; nothing was aborted".to_string(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            "no MERGE_HEAD was left; nothing to abort".to_string()
        }
        Err(e) => format!("MERGE_HEAD could not be read ({e}); nothing was aborted"),
    };
    Err(ctx(format!(
        "`git merge` of the row exited {} ({aborted}): {}{}",
        r.code,
        r.stdout.trim(),
        r.stderr.trim()
    )))
}
