//! Deterministic git-worktree lifecycle. Wraps `git worktree` so the skill
//! never hand-rolls these commands (and so the invariants — path outside the
//! repo, one branch per dir, clean removal — are enforced in code, not prose).

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use wait_timeout::ChildExt;

use crate::config::Config;
use crate::lock;

/// Every `git` subprocess spawned by this module is bounded by this timeout.
/// worktree lifecycle ops (add/remove/merge/status) are expected to be local
/// and fast, but a hung `git` (lock contention, a stuck credential helper
/// prompt, a network-mounted repo, corrupted pack, etc.) must not block a
/// condukt run indefinitely. 45s is generous for any of the local ops this
/// module performs while still bounding worst-case hangs.
const GIT_TIMEOUT: Duration = Duration::from_secs(45);

/// Output captured from a (possibly timed-out) git invocation.
struct GitOutput {
    status: Option<std::process::ExitStatus>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
}

/// Spawn `git <args>` in `dir` and wait up to [`GIT_TIMEOUT`], capturing both
/// streams. On timeout the child (and, on Unix, its process group) is killed
/// so no orphaned `git` process is left running, and `timed_out` is set so
/// callers can produce an actionable error instead of silently returning
/// empty output.
fn run_git_bounded(dir: &Path, args: &[&str]) -> Result<GitOutput> {
    run_git_bounded_with(Path::new("git"), dir, args, GIT_TIMEOUT)
}

/// Same as [`run_git_bounded`] but with the git binary path and timeout as
/// parameters, so tests can point it at a fake script (a stand-in for a
/// hung/slow real `git`) with a short local timeout override — instead of
/// mutating process-global state like `PATH` (which would race with the many
/// other tests in this module that shell out to the real `git` concurrently
/// under `cargo test`'s multi-threaded runner) or waiting out the full
/// production [`GIT_TIMEOUT`].
fn run_git_bounded_with(
    git_bin: &Path,
    dir: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<GitOutput> {
    let mut cmd = Command::new(git_bin);
    cmd.current_dir(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn git {:?}", args))?;

    match child.wait_timeout(timeout) {
        Ok(Some(status)) => {
            let stdout = read_bounded(child.stdout.take(), timeout);
            let stderr = read_bounded(child.stderr.take(), timeout);
            Ok(GitOutput {
                status: Some(status),
                stdout,
                stderr,
                timed_out: false,
            })
        }
        Ok(None) => {
            kill_tree(&mut child);
            let _ = child.wait();
            Ok(GitOutput {
                status: None,
                stdout: Vec::new(),
                stderr: Vec::new(),
                timed_out: true,
            })
        }
        Err(e) => Err(anyhow!("failed to wait on git {:?}: {e}", args)),
    }
}

/// Kill the whole process tree of a timed-out `git` call, not just the direct
/// process, mirroring the same group-kill approach used elsewhere in this
/// workspace (propguard::git, autoflow::compass) for hung subprocesses.
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as i32;
        // SAFETY: plain libc syscall; negative pid targets the process group
        // created via `process_group(0)` at spawn time. Best effort: any
        // error is ignored, same as the plain `child.kill()` it supplements.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

/// Bounded stream read: never block past `timeout` even if some lingering
/// process keeps the pipe's write end open after the immediate child exits.
fn read_bounded<R: std::io::Read + Send + 'static>(
    stream: Option<R>,
    timeout: Duration,
) -> Vec<u8> {
    use std::sync::mpsc;
    let Some(mut s) = stream else {
        return Vec::new();
    };
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out);
        let _ = tx.send(out);
    });
    rx.recv_timeout(timeout).unwrap_or_default()
}

/// Run `git` with args in `dir`, returning trimmed stdout. On failure the error
/// preserves git's exit status and BOTH output streams: git writes diagnostics
/// to stderr but also to stdout (merge CONFLICT lines, `branch -d` refusals), so
/// dropping either can hide the root cause. Callers add `.with_context()` to name
/// the lifecycle op (create/merge/remove); the chain then reads
/// "<op> failed: git [..] exited <code>: <stderr>/<stdout>". Bounded by
/// [`GIT_TIMEOUT`] — a hung git process is killed and reported as a timeout
/// error rather than blocking forever.
pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    git_output_to_result(run_git_bounded(dir, args)?, dir, args, GIT_TIMEOUT)
}

/// Shared formatting step for [`git`]: turn a (possibly timed-out) raw
/// `GitOutput` into the same `Result<String>` shape, given the timeout that
/// was actually used (so error messages/tests can use a short override
/// instead of the hardcoded [`GIT_TIMEOUT`]).
fn git_output_to_result(
    out: GitOutput,
    dir: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<String> {
    if out.timed_out {
        bail!(
            "git {:?} in {} timed out after {:?} and was killed",
            args,
            dir.display(),
            timeout
        );
    }
    let status = out
        .status
        .expect("status is Some when not timed_out (run_git_bounded invariant)");
    if !status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut detail = stderr.trim().to_string();
        let so = stdout.trim();
        if !so.is_empty() {
            // Keep stdout too — some git errors only surface there.
            if detail.is_empty() {
                detail = so.to_string();
            } else {
                detail.push_str("\n--- git stdout ---\n");
                detail.push_str(so);
            }
        }
        if detail.is_empty() {
            detail = "(git produced no output)".to_string();
        }
        bail!(
            "git {:?} in {} exited {}: {}",
            args,
            dir.display(),
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".into()),
            detail
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Repo root for `cwd` per git itself.
pub fn toplevel(cwd: &Path) -> Result<PathBuf> {
    let s = git(cwd, &["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(s))
}

/// Is `branch` already checked out in some worktree of this repo?
fn branch_checked_out(repo: &Path, branch: &str) -> Result<bool> {
    let listing = git(repo, &["worktree", "list", "--porcelain"])?;
    let needle = format!("branch refs/heads/{branch}");
    Ok(listing.lines().any(|l| l.trim() == needle))
}

/// Validate a `topic` — a single path component appended to `worktree_base`.
/// Rejects anything that could traverse out of the base or be parsed by git as
/// an option: empty, a leading `-`/`.`, embedded `..`, or any char outside
/// `[A-Za-z0-9._-]` (notably path separators). `topic`/`branch` are derived from
/// LLM-authored task names, so they are untrusted input.
fn validate_topic(topic: &str) -> Result<()> {
    if topic.is_empty() {
        bail!("worktree topic must not be empty");
    }
    if topic.starts_with('-') || topic.starts_with('.') {
        bail!("worktree topic {topic:?} must not start with '-' or '.'");
    }
    if topic.contains("..") {
        bail!("worktree topic {topic:?} must not contain '..'");
    }
    if !topic
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        bail!("worktree topic {topic:?} may only contain [A-Za-z0-9._-] (no path separators)");
    }
    Ok(())
}

/// Validate a `branch` name. Allows `/` (git refs like `condukt/t2`) but rejects
/// a leading `-` (git option injection), a leading/trailing `/`, `..`/`//`, and
/// any char outside `[A-Za-z0-9._/-]`.
fn validate_branch(branch: &str) -> Result<()> {
    if branch.is_empty() {
        bail!("branch must not be empty");
    }
    if branch.starts_with('-') || branch.starts_with('/') {
        bail!("branch {branch:?} must not start with '-' or '/'");
    }
    if branch.ends_with('/') {
        bail!("branch {branch:?} must not end with '/'");
    }
    if branch.contains("..") || branch.contains("//") {
        bail!("branch {branch:?} must not contain '..' or '//'");
    }
    if !branch
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
    {
        bail!("branch {branch:?} may only contain [A-Za-z0-9._/-]");
    }
    Ok(())
}

/// Canonicalize `path`, falling back to canonicalizing the nearest existing
/// ancestor and rejoining the non-existent trailing components when `path`
/// itself does not exist yet (the common case here: we are about to create a
/// worktree dir that doesn't exist). Mirrors the same "canonicalize what
/// exists, join the rest" shape used in `harness-core::store::write_note_named`
/// so a raw (non-canonical) prefix — a symlinked temp dir, a WSL/DrvFs mount —
/// can't make an identity/prefix check disagree with what git will actually see.
fn canonicalize_prefix(path: &Path) -> PathBuf {
    if let Ok(c) = path.canonicalize() {
        return c;
    }
    // Walk up from `path` one component at a time until we find an ancestor
    // that exists and can be canonicalized, then rejoin the non-existent
    // trailing part (relative to that ancestor) onto the canonical form.
    let mut ancestor = path.to_path_buf();
    while ancestor.pop() {
        if let Ok(canon_ancestor) = ancestor.canonicalize() {
            let rel = path.strip_prefix(&ancestor).unwrap_or(Path::new(""));
            return canon_ancestor.join(rel);
        }
    }
    // No existing ancestor found at all (exhausted all components); fall
    // back to the original, uncanonicalized path.
    path.to_path_buf()
}

/// Create a worktree at `<worktree_base>/<topic>` on a new `branch`.
/// Enforces: topic/branch are sanitized, path is outside the repo, and the
/// branch isn't already checked out.
pub fn create(repo: &Path, worktree_base: &Path, topic: &str, branch: &str) -> Result<PathBuf> {
    validate_topic(topic)?;
    validate_branch(branch)?;

    let repo_canon = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    let path = worktree_base.join(topic);

    // Canonicalize `path`'s existing ancestor (the path itself does not exist
    // yet — it's about to be created — so `path.canonicalize()` would fail)
    // before the prefix check below. Without this, an un-canonical
    // `worktree_base` (e.g. reached through a symlink, as on macOS
    // `/tmp` -> `/private/tmp`, or a WSL/DrvFs mount) could make `path` look
    // like it's outside `repo_canon` via `starts_with` even when it actually
    // resolves inside the repo (TOCTOU-adjacent: the check and the later
    // `git worktree add` could disagree on identity). We canonicalize the
    // nearest existing ancestor and rejoin the non-existent suffix so the
    // comparison is done on two canonical paths.
    let path_canon = canonicalize_prefix(&path);

    if path_canon.starts_with(&repo_canon) {
        bail!(
            "refusing to create worktree inside the repo ({}); set worktree_base outside it",
            path.display()
        );
    }
    if path.exists() {
        bail!("worktree path already exists: {}", path.display());
    }
    // Prune stale worktree registrations before the branch-checked-out gate.
    // If a worktree dir was removed out-of-band, git keeps a lingering admin
    // entry that still reports the branch as "checked out" — falsely blocking a
    // re-cut of the same branch until someone runs prune by hand. Pruning first
    // clears only *dropped* (dir-gone) registrations; a branch genuinely checked
    // out in a LIVE worktree survives prune and is still correctly rejected
    // below. Best-effort / fail-soft, matching `discard()`'s prune idiom.
    //
    // `git worktree prune` and the branch-ref mutations that follow (branch -D,
    // worktree add) all mutate the ONE primary repo, so take the repo-scoped
    // lock to serialize with a concurrent merge/prune in another run. Held to
    // the end of create(). (`_loaded`: create() doesn't thread a Config and its
    // callers live in sibling modules out of this change's scope.)
    let _repo_lock = lock::acquire_repo_primary_loaded(repo);
    let _ = git(repo, &["worktree", "prune"]);
    if branch_checked_out(repo, branch)? {
        bail!(
            "branch '{branch}' is already checked out in another worktree (one dir = one branch)"
        );
    }

    // Pruning above cleared any *dropped* worktree's stale admin registration,
    // and the branch_checked_out() gate above has already rejected a branch that
    // is genuinely LIVE in another (dir-present) worktree. But `git worktree
    // prune` keeps the branch REF itself (its commits are not garbage), so a
    // stale leftover ref from a dropped/abandoned attempt can still exist here.
    // A fresh `-b` (create-new-branch) would then hard-fail with "a branch named
    // '<branch>' already exists".
    //
    // Force-delete that lingering ref so every re-cut is deterministically
    // PRISTINE — branched off the caller's base HEAD — instead of silently
    // re-attaching a crashed attempt's commits. condukt reuses `condukt/<t.id>`
    // on every retry; the Abandon/stuck-worker recovery path only resets
    // run-state pointers and never deletes the git branch, so a prior attempt's
    // commits survive on the ref. That prior-attempt state is meant to flow
    // explicitly via failure_context.diff into a NEW worktree, not invisibly
    // through a reused ref (which would also corrupt the post-execution diffrisk
    // audit — lines_added would count commits the current worker never wrote).
    // This mirrors discard()'s "throw the branch away" (`git branch -D`) idiom.
    //
    // CRITICAL GUARD: this is safe precisely because we only reach here once the
    // branch is NOT live anywhere. `prune` only clears dir-missing registrations,
    // so a branch still checked out in a live worktree makes branch_checked_out()
    // == true above and is rejected before this point; a surviving ref with
    // branch_checked_out() == false is therefore a stale leftover, never a live
    // branch. We never `-D` a branch that is checked out in a live worktree.
    let (branch_ref_exists, _, _) = git_try(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )?;
    if branch_ref_exists {
        git(repo, &["branch", "-D", branch]).with_context(|| {
            format!("could not force-delete stale branch ref '{branch}' before re-cut")
        })?;
    }

    if let Some(parent) = path.parent() {
        // Loud-fail: if the worktree's parent can't be created, `git worktree add`
        // below would otherwise fail with a confusing path error. Name the dir.
        std::fs::create_dir_all(parent).with_context(|| {
            format!("could not create worktree parent dir {}", parent.display())
        })?;
    }
    let path_str = path.to_string_lossy().to_string();
    // Always create a fresh branch off the current base HEAD. `--` ends option
    // parsing so a path/branch can never be read as a flag (defense in depth on
    // top of validate_topic/validate_branch above).
    git(repo, &["worktree", "add", "-b", branch, "--", &path_str]).with_context(|| {
        format!("could not create worktree for branch '{branch}' at {path_str}")
    })?;
    Ok(path)
}

/// Run a git command without bailing on non-zero exit; return (success, stdout, stderr).
/// Bounded by [`GIT_TIMEOUT`] like [`git`]: a hung git process is killed and
/// reported as a failed (non-success) result carrying a timeout message on
/// stderr, rather than blocking forever.
fn git_try(dir: &Path, args: &[&str]) -> Result<(bool, String, String)> {
    let out = run_git_bounded(dir, args)?;
    if out.timed_out {
        return Ok((
            false,
            String::new(),
            format!(
                "git {:?} in {} timed out after {:?} and was killed",
                args,
                dir.display(),
                GIT_TIMEOUT
            ),
        ));
    }
    let status = out
        .status
        .expect("status is Some when not timed_out (run_git_bounded invariant)");
    Ok((
        status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    ))
}

/// Merge `branch` into the configured default branch. Verifies the repo is
/// actually on the default branch first (verify -> act).
///
/// Pre-flight: attempts `git merge --no-commit --no-ff` to detect conflicts
/// before performing the real merge. If conflicts are detected, aborts the
/// trial merge and returns an error without touching the branch history.
pub fn merge(cfg: &Config, repo: &Path, branch: &str, default_branch: &str) -> Result<()> {
    // Serialize with every other primary-repo default-branch mutator (another
    // condukt run's merge / main-tree commit / `git worktree prune`) on ONE
    // repo-scoped advisory lock, so two runs in the same repo never race on the
    // default branch. Held for the entire checkout+merge critical section and
    // released on drop. Fail-soft: `acquire_repo_primary` never panics.
    let _repo_lock = lock::acquire_repo_primary(cfg, repo);
    git(repo, &["checkout", default_branch])
        .with_context(|| format!("could not checkout {default_branch} before merge"))?;
    let current = git(repo, &["branch", "--show-current"])?;
    if current != default_branch {
        bail!("expected to be on '{default_branch}' but on '{current}'; aborting merge");
    }

    // ── Pre-flight: trial merge (no-commit) to detect conflicts ──────────────
    // `git merge --no-commit --no-ff` either succeeds with a staged merge or
    // fails immediately when conflicts exist. In both cases we abort and then
    // re-run the real merge only when there are no conflicts.
    let (trial_ok, _, trial_stderr) = git_try(repo, &["merge", "--no-commit", "--no-ff", branch])?;

    if !trial_ok {
        // Trial merge reported conflicts. Abort to restore clean state.
        let _ = git_try(repo, &["merge", "--abort"]);
        bail!(
            "merge of '{branch}' into '{default_branch}' has conflicts (pre-flight); \
             aborting without modifying history.\n{trial_stderr}"
        );
    }

    // Even a "successful" --no-commit merge may leave CONFLICT markers when
    // git decides to apply both sides with markers rather than refusing. Use
    // `git ls-files --unmerged` to catch those cases.
    let unmerged = git(repo, &["ls-files", "--unmerged"])?;
    if !unmerged.trim().is_empty() {
        let _ = git_try(repo, &["merge", "--abort"]);
        bail!(
            "merge of '{branch}' into '{default_branch}' has unresolved conflicts (pre-flight); \
             aborting without modifying history."
        );
    }

    // No conflicts found in trial — abort the staged trial and do the real merge.
    git_try(repo, &["merge", "--abort"])
        .with_context(|| "could not abort trial merge before real merge")?;

    git(repo, &["merge", "--no-edit", branch])
        .with_context(|| format!("merge of {branch} into {default_branch} failed"))?;
    Ok(())
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod worktree_remove_tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Initialise a bare-minimum git repo with an initial commit on `main`.
    fn init_repo() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().expect("tempdir");
        let repo = tmp.path().to_path_buf();

        git(&repo, &["init", "-b", "main"]).unwrap();
        git(&repo, &["config", "user.email", "test@example.com"]).unwrap();
        git(&repo, &["config", "user.name", "Test"]).unwrap();

        // Initial commit on main
        let f = repo.join("base.txt");
        fs::write(&f, "base\n").unwrap();
        git(&repo, &["add", "."]).unwrap();
        git(&repo, &["commit", "-m", "init"]).unwrap();

        (tmp, repo)
    }

    /// Minimal `Config` for merge tests: its only functional use here is the
    /// `state_dir` where `acquire_repo_primary` publishes the repo-scoped lock,
    /// pinned under `tmp` so tests never touch the real state dir.
    fn test_cfg(tmp: &Path) -> Config {
        Config {
            worktree_base: tmp.join("worktrees"),
            default_branch: "main".to_string(),
            shared_globs: Vec::new(),
            max_parallel: 4,
            state_dir: tmp.join("state"),
            test_command: None,
            stuck_ttl_secs: 1800,
            build_command: None,
            deploy_command: None,
            loop_max_iters: 10,
            autonomous: false,
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

    /// Create a branch from HEAD, write `content` to `file`, commit and return.
    fn make_branch(repo: &Path, branch: &str, file: &str, content: &str) {
        git(repo, &["checkout", "-b", branch]).unwrap();
        fs::write(repo.join(file), content).unwrap();
        git(repo, &["add", "."]).unwrap();
        git(repo, &["commit", "-m", &format!("add {file} on {branch}")]).unwrap();
        git(repo, &["checkout", "main"]).unwrap();
    }

    #[test]
    fn worktree_merge_no_conflict_succeeds() {
        let (_tmp, repo) = init_repo();
        make_branch(&repo, "feat", "feat.txt", "feature content\n");

        let cfg = test_cfg(&repo);
        merge(&cfg, &repo, "feat", "main").expect("clean merge should succeed");

        // The file should now exist on main
        assert!(repo.join("feat.txt").exists());
    }

    #[test]
    fn worktree_merge_conflict_returns_error() {
        let (_tmp, repo) = init_repo();

        // Both branches modify the same file at the same line → guaranteed conflict
        let conflict_file = "shared.txt";

        // Write a shared base first on main
        fs::write(repo.join(conflict_file), "line1\nline2\nline3\n").unwrap();
        git(&repo, &["add", "."]).unwrap();
        git(&repo, &["commit", "-m", "add shared file"]).unwrap();

        // Branch: modify line2 to "branch version"
        git(&repo, &["checkout", "-b", "conflict-branch"]).unwrap();
        fs::write(repo.join(conflict_file), "line1\nbranch version\nline3\n").unwrap();
        git(&repo, &["add", "."]).unwrap();
        git(&repo, &["commit", "-m", "branch edit"]).unwrap();

        // Main: modify line2 differently → creates a real conflict
        git(&repo, &["checkout", "main"]).unwrap();
        fs::write(repo.join(conflict_file), "line1\nmain version\nline3\n").unwrap();
        git(&repo, &["add", "."]).unwrap();
        git(&repo, &["commit", "-m", "main edit"]).unwrap();

        let cfg = test_cfg(&repo);
        let result = merge(&cfg, &repo, "conflict-branch", "main");
        assert!(
            result.is_err(),
            "conflicting merge should return an error, but got Ok"
        );

        let err_msg = format!("{:?}", result.unwrap_err());
        assert!(
            err_msg.contains("conflict") || err_msg.contains("unresolved"),
            "error message should mention conflicts, got: {err_msg}"
        );

        // The repo should be in a clean state (no in-progress merge)
        let merge_head = repo.join(".git").join("MERGE_HEAD");
        assert!(
            !merge_head.exists(),
            "MERGE_HEAD should not exist after aborted pre-flight"
        );
    }

    #[test]
    fn worktree_is_dirty_clean_repo() {
        let (_tmp, repo) = init_repo();
        assert!(!is_dirty(&repo).expect("is_dirty should not error on a clean repo"));
    }

    #[test]
    fn worktree_is_dirty_with_uncommitted_change() {
        let (_tmp, repo) = init_repo();
        fs::write(repo.join("new.txt"), "dirty\n").unwrap();
        assert!(is_dirty(&repo).expect("is_dirty should not error"));
    }

    /// remove() force-removes a DIRTY worktree so orphans do not accumulate.
    /// A plain `git worktree remove` refuses when the worktree has uncommitted
    /// or untracked files; without a force-retry this returns Err and the dir is
    /// never cleaned up (the unattended/parallel failure mode).
    #[test]
    fn worktree_remove_force_removes_dirty_worktree() {
        let (tmp, repo) = init_repo();

        let wt_base = tmp.path().join("worktrees");
        fs::create_dir_all(&wt_base).unwrap();
        let wt_path = wt_base.join("dirty-wt");
        git(
            &repo,
            &[
                "worktree",
                "add",
                wt_path.to_str().unwrap(),
                "-b",
                "feat/dirty",
            ],
        )
        .unwrap();

        // Make the worktree DIRTY: an untracked file inside the worktree dir.
        fs::write(wt_path.join("uncommitted.txt"), "dirty\n").unwrap();
        assert!(is_dirty(&wt_path).unwrap(), "worktree should be dirty");

        // remove() must succeed on a dirty worktree (force) and leave no orphan.
        let result = remove(&repo, &wt_path, None).expect("dirty worktree should be force-removed");
        assert_eq!(result, None, "no branch requested -> None");
        assert!(
            !wt_path.exists(),
            "dirty worktree dir must be gone after remove(); got orphan at {}",
            wt_path.display()
        );
    }
}

/// Remove the worktree at `path` and delete its `branch` (best-effort on branch).
///
/// If the branch cannot be deleted because it is not fully merged, a warning is
/// printed to stderr and the function still returns `Ok(())`. The caller is
/// responsible for acting on the warning (e.g. the CLI prints it again with
/// context).
///
/// Returns `Some(branch_name)` when the branch was NOT deleted (unmerged or
/// any other error), `None` when no branch was requested or deletion succeeded.
pub fn remove(repo: &Path, path: &Path, branch: Option<&str>) -> Result<Option<String>> {
    let path_str = path.to_string_lossy().to_string();
    // Try a clean remove first. `git worktree remove` REFUSES when the worktree
    // has uncommitted or untracked files (and for a few other reasons); in
    // unattended/parallel runs that would leave orphan dirs accumulating on disk.
    // On failure, retry with --force. Force-remove only discards uncommitted
    // worktree state — committed work lives on the branch and is handled by the
    // best-effort branch-deletion step below, so this is safe for cleanup.
    if git(repo, &["worktree", "remove", &path_str]).is_err() {
        git(repo, &["worktree", "remove", "--force", &path_str])
            .with_context(|| format!("could not force-remove worktree {}", path.display()))?;
    }
    if let Some(b) = branch {
        match git(repo, &["branch", "-d", b]) {
            Ok(_) => {}
            Err(e) => {
                // The branch still exists — most likely it was not fully merged.
                // Warn on stderr and surface the branch name to the caller so it
                // can display a more actionable message.
                eprintln!(
                    "warning: branch '{}' was not deleted (not fully merged). \
                     Use `git branch -D {}` to force-delete, or merge it first.",
                    b, b
                );
                eprintln!("  (git said: {e})");
                return Ok(Some(b.to_string()));
            }
        }
    }
    Ok(None)
}

/// Discard the worktree at `path` (force-remove) and FORCE-DELETE its `branch`
/// (`git branch -D`), dropping any unmerged commits.
///
/// Unlike [`remove`] (which uses `branch -d` and *preserves* an unmerged
/// branch), `discard` intentionally throws the branch away: it is the
/// disposal path for an experiment worktree whose value is the learning
/// artifact, not the code. Callers MUST capture the branch SHA / diff before
/// calling this — once the branch is `-D`'d the commits are unreferenced.
///
/// If the worktree dir is already gone we `worktree prune` first so the stale
/// admin entry does not make `branch -D` refuse ("checked out").
pub fn discard(repo: &Path, path: &Path, branch: Option<&str>) -> Result<()> {
    // Serialize the worktree-remove / `git worktree prune` / branch -D below
    // (all primary-repo mutations) with a concurrent merge/prune in another run
    // on the shared repo-scoped lock. Held for the whole function; fail-soft.
    // (`_loaded`: discard() doesn't thread a Config; callers are in sibling
    // modules out of this change's scope.)
    let _repo_lock = lock::acquire_repo_primary_loaded(repo);
    let path_str = path.to_string_lossy().to_string();
    if path.exists() {
        if git(repo, &["worktree", "remove", &path_str]).is_err() {
            git(repo, &["worktree", "remove", "--force", &path_str])
                .with_context(|| format!("could not force-remove worktree {}", path.display()))?;
        }
    } else {
        // Dir already removed out-of-band — drop the stale worktree registration
        // so the branch is no longer considered checked out.
        let _ = git(repo, &["worktree", "prune"]);
    }
    if let Some(b) = branch {
        git(repo, &["branch", "-D", b])
            .with_context(|| format!("could not force-delete branch '{b}' during discard"))?;
    }
    Ok(())
}

/// (path, branch) pairs for every registered worktree except the primary.
pub fn list(repo: &Path) -> Result<Vec<(PathBuf, Option<String>)>> {
    let listing = git(repo, &["worktree", "list", "--porcelain"])?;
    // `.ok()` is intentional here and below: a worktree dir that was already
    // removed (the common cleanup case) cannot be canonicalized, and `None` is a
    // valid "not the primary" outcome for the equality check. This is NOT a
    // swallowed error to loud-fail — unlike the mkdir paths in create()/init().
    let primary = toplevel(repo)?.canonicalize().ok();
    let mut out = Vec::new();
    let mut cur_path: Option<PathBuf> = None;
    let mut cur_branch: Option<String> = None;
    for line in listing.lines().chain(std::iter::once("")) {
        if let Some(p) = line.strip_prefix("worktree ") {
            // flush previous
            if let Some(path) = cur_path.take() {
                let is_primary = path.canonicalize().ok() == primary;
                if !is_primary {
                    out.push((path, cur_branch.take()));
                }
            }
            cur_branch = None;
            cur_path = Some(PathBuf::from(p));
        } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
            cur_branch = Some(b.to_string());
        } else if line.is_empty() {
            if let Some(path) = cur_path.take() {
                let is_primary = path.canonicalize().ok() == primary;
                if !is_primary {
                    out.push((path, cur_branch.take()));
                }
            }
        }
    }
    Ok(out)
}

/// Resolve the `gitdir:` a worktree-style `.git` *file* points at, if any.
///
/// A git worktree's `.git` is a plain-text file (not a directory) containing
/// a single line `gitdir: <path>`. The path is typically absolute already,
/// but may be relative to `git_file`'s parent when git wrote it that way, so
/// both are handled. Returns `None` if `git_file` isn't a `.git` file, isn't
/// readable, or doesn't match the expected `gitdir: ...` shape.
fn resolve_gitdir_pointer(git_file: &Path) -> Option<PathBuf> {
    let contents = std::fs::read_to_string(git_file).ok()?;
    let line = contents.lines().next()?.trim();
    let raw = line.strip_prefix("gitdir:")?.trim();
    let candidate = PathBuf::from(raw);
    let resolved = if candidate.is_absolute() {
        candidate
    } else {
        git_file.parent()?.join(candidate)
    };
    Some(resolved)
}

/// Which repository (by its canonicalized `.git` common dir) a directory
/// under `worktree_base` actually belongs to, if it belongs to any repo at
/// all.
///
/// - A `.git` *file* (worktree pointer) resolves to the target's own
///   `.git/worktrees/<name>` dir; its parent-of-parent is the owning repo's
///   `.git` common dir.
/// - A `.git` *directory* (a plain clone dropped into `worktree_base`, not a
///   linked worktree) resolves to itself.
/// - No `.git` at all (or an unreadable/malformed pointer) yields `None` —
///   the caller treats that conservatively as condukt-created debris rather
///   than silently ignoring it.
fn owning_repo_git_dir(candidate: &Path) -> Option<PathBuf> {
    let dot_git = candidate.join(".git");
    if dot_git.is_dir() {
        return dot_git.canonicalize().ok();
    }
    if dot_git.is_file() {
        let pointed = resolve_gitdir_pointer(&dot_git)?;
        // A linked worktree's gitdir is `<repo>/.git/worktrees/<name>`; the
        // owning repo's common `.git` dir is two levels up. Canonicalize the
        // pointed-at dir first (it may itself contain no further symlinks
        // but could be relative-normalized oddly) then walk up.
        let pointed = pointed.canonicalize().ok().unwrap_or(pointed);
        return pointed
            .parent() // .git/worktrees
            .and_then(|p| p.parent()) // .git
            .map(|p| p.to_path_buf())
            .and_then(|p| p.canonicalize().ok().or(Some(p)));
    }
    None
}

/// Worktree dirs physically under `worktree_base` that git no longer tracks
/// **for `repo`**.
///
/// `worktree_base` may be shared across multiple, unrelated repositories on
/// the same machine (a common setup when several projects park their
/// worktrees under the same scratch dir). A candidate directory is only
/// reported as an orphan if it actually belongs to `repo` (its `.git`
/// resolves under `repo`'s own `.git` common dir) and isn't in `repo`'s
/// registered worktree list. A directory whose `.git` resolves to a
/// *different* repo is silently skipped — it's not condukt's concern.
/// A directory with no `.git` at all is treated conservatively as an orphan
/// (it really is unattributable debris, most likely condukt-created).
pub fn orphans(repo: &Path, worktree_base: &Path) -> Result<Vec<PathBuf>> {
    if !worktree_base.exists() {
        return Ok(Vec::new());
    }
    let registered: Vec<PathBuf> = list(repo)?
        .into_iter()
        .filter_map(|(p, _)| p.canonicalize().ok())
        .collect();
    let repo_git_dir = git(
        repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .ok()
    .map(PathBuf::from)
    .and_then(|p| p.canonicalize().ok());
    let mut orphans = Vec::new();
    for entry in std::fs::read_dir(worktree_base)
        .with_context(|| format!("reading {}", worktree_base.display()))?
    {
        let path = entry?.path();
        if !path.is_dir() {
            continue;
        }
        let canon = path.canonicalize().unwrap_or_else(|_| path.clone());
        if registered.contains(&canon) {
            continue;
        }
        match owning_repo_git_dir(&path) {
            Some(owner) => {
                // Only flag it if it belongs to `repo` itself; a directory
                // owned by some other repo sharing this worktree_base is not
                // condukt's concern and is silently skipped.
                if repo_git_dir.as_ref() == Some(&owner) {
                    orphans.push(path);
                }
            }
            // No `.git` at all (or malformed): conservative — condukt debris.
            None => orphans.push(path),
        }
    }
    Ok(orphans)
}

/// Does a worktree have uncommitted changes (tracked or untracked)?
pub fn is_dirty(path: &Path) -> Result<bool> {
    let status = git(path, &["status", "--porcelain"])
        .map_err(|e| anyhow!("status check failed for {}: {e}", path.display()))?;
    Ok(!status.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Initialise a bare-minimum git repo in `dir` (main branch, one commit).
    fn init_repo(dir: &Path) {
        git(dir, &["init", "-b", "main"]).unwrap();
        git(dir, &["config", "user.email", "test@example.com"]).unwrap();
        git(dir, &["config", "user.name", "Test"]).unwrap();
        // Commit something so HEAD is valid.
        let readme = dir.join("README.md");
        fs::write(&readme, "initial").unwrap();
        git(dir, &["add", "README.md"]).unwrap();
        git(dir, &["commit", "-m", "initial"]).unwrap();
    }

    #[test]
    fn validate_topic_accepts_safe_and_rejects_dangerous() {
        // Safe single-component topics.
        assert!(validate_topic("t2").is_ok());
        assert!(validate_topic("fix-bug_3.1").is_ok());
        // Dangerous: traversal, separators, option injection, empty, dotfiles.
        assert!(validate_topic("").is_err());
        assert!(validate_topic("..").is_err());
        assert!(validate_topic("../evil").is_err());
        assert!(
            validate_topic("a/b").is_err(),
            "path separator must be rejected"
        );
        assert!(
            validate_topic("-rf").is_err(),
            "leading '-' must be rejected"
        );
        assert!(validate_topic(".hidden").is_err());
        assert!(validate_topic("a b").is_err(), "spaces must be rejected");
        assert!(validate_topic("a;rm -rf").is_err());
    }

    #[test]
    fn git_error_preserves_git_diagnostic() {
        // A non-repo dir makes git fail with a recognisable diagnostic on stderr.
        // The error must surface git's own message (root cause) plus the dir, not
        // a bare "git failed", so create/merge/remove failures are debuggable.
        let tmp = tempfile::tempdir().expect("tempdir");
        let err = git(tmp.path(), &["rev-parse", "--show-toplevel"]).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("not a git repository") || msg.contains("fatal"),
            "must carry git's own diagnostic, got: {msg}"
        );
        assert!(
            msg.contains(&tmp.path().display().to_string()),
            "must name the dir git ran in, got: {msg}"
        );
        assert!(
            !msg.contains("(git produced no output)"),
            "git emitted to stderr; detail must not be the empty-output placeholder"
        );
    }

    #[test]
    fn validate_branch_allows_slashes_but_blocks_injection() {
        // Real branch names used by condukt.
        assert!(validate_branch("condukt/t2").is_ok());
        assert!(validate_branch("feature/x.y_z-1").is_ok());
        // Dangerous forms.
        assert!(validate_branch("").is_err());
        assert!(
            validate_branch("-b").is_err(),
            "leading '-' must be rejected"
        );
        assert!(validate_branch("/abs").is_err());
        assert!(validate_branch("trailing/").is_err());
        assert!(validate_branch("a..b").is_err());
        assert!(validate_branch("a//b").is_err());
        assert!(validate_branch("a b").is_err());
    }

    /// create() rejects an unsafe topic before invoking git.
    #[test]
    fn create_rejects_traversal_topic() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        let base = tmp.path().join("wt");
        let err = create(&repo, &base, "../escape", "condukt/x").unwrap_err();
        assert!(err.to_string().contains("topic"));
    }

    /// create() loud-fails (does not silently swallow) when the worktree parent
    /// dir cannot be created. A regular file standing where a dir must be makes
    /// `create_dir_all` fail deterministically — uid-independent, unlike a chmod
    /// read-only trick that root would bypass in CI.
    #[test]
    fn create_loud_fails_when_parent_dir_uncreatable() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        // `blocker` is a FILE; using it as a path component forces mkdir to fail.
        let blocker = tmp.path().join("blocker");
        fs::write(&blocker, b"not a dir").unwrap();
        let worktree_base = blocker.join("sub"); // parent (a file) can't be mkdir'd
        let err = create(&repo, &worktree_base, "topic", "condukt/x").unwrap_err();
        // The {:#} alt form walks the anyhow chain so our .context() is visible.
        let chain = format!("{err:#}");
        assert!(
            chain.contains("worktree parent dir"),
            "must surface the loud parent-dir context, got: {chain}"
        );
    }

    /// remove() returns Ok(None) when the branch was merged and deletes cleanly.
    #[test]
    fn remove_returns_none_when_branch_merged() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);

        // Create a branch that we immediately merge back in — it is "merged".
        git(&repo, &["checkout", "-b", "feat/merged"]).unwrap();
        git(&repo, &["checkout", "main"]).unwrap();
        git(&repo, &["merge", "--no-edit", "feat/merged"]).unwrap();

        // Re-create the branch so we can try to remove it via add+remove worktree.
        // The branch has no unique commits so -d will succeed.
        // To avoid checking out the branch in a worktree we just test branch -d
        // directly via the expected code path: a branch that is fully merged.
        // We simulate by calling remove() with a non-existent path (after prune)
        // We need a real worktree for the `git worktree remove` to work, so:
        let wt_base = tmp.path().join("worktrees");
        fs::create_dir_all(&wt_base).unwrap();
        let wt_path = wt_base.join("merged-wt");
        // Create a new branch for the worktree (identical content to main).
        git(
            &repo,
            &[
                "worktree",
                "add",
                wt_path.to_str().unwrap(),
                "-b",
                "feat/wt-merged",
            ],
        )
        .unwrap();
        // Merge it.
        git(&repo, &["merge", "--no-edit", "feat/wt-merged"]).unwrap();
        // Now remove: branch -d should succeed because it is merged.
        let result = remove(&repo, &wt_path, Some("feat/wt-merged")).unwrap();
        assert_eq!(
            result, None,
            "merged branch should be deleted: got {:?}",
            result
        );
    }

    /// remove() returns Ok(Some(branch)) when the branch is NOT merged.
    #[test]
    fn remove_returns_branch_name_when_unmerged() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);

        let wt_base = tmp.path().join("worktrees");
        fs::create_dir_all(&wt_base).unwrap();
        let wt_path = wt_base.join("unmerged-wt");
        git(
            &repo,
            &[
                "worktree",
                "add",
                wt_path.to_str().unwrap(),
                "-b",
                "feat/unmerged",
            ],
        )
        .unwrap();

        // Add a commit to the worktree so it diverges from main.
        let extra = wt_path.join("extra.txt");
        fs::write(&extra, "unique").unwrap();
        git(&wt_path, &["add", "extra.txt"]).unwrap();
        git(&wt_path, &["commit", "-m", "diverge"]).unwrap();

        // Now remove: -d should fail because feat/unmerged is not merged.
        let result = remove(&repo, &wt_path, Some("feat/unmerged")).unwrap();
        assert_eq!(
            result.as_deref(),
            Some("feat/unmerged"),
            "unmerged branch should be returned"
        );

        // The branch must still exist in the repo.
        let branches = git(&repo, &["branch"]).unwrap();
        assert!(
            branches.contains("feat/unmerged"),
            "unmerged branch should still exist after remove; got: {}",
            branches
        );
    }

    /// orphans() returns directories under worktree_base not tracked by git.
    #[test]
    fn orphans_detects_unregistered_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);

        let wt_base = tmp.path().join("worktrees");
        fs::create_dir_all(&wt_base).unwrap();

        // A directory that git never knew about.
        let ghost = wt_base.join("ghost-dir");
        fs::create_dir_all(&ghost).unwrap();

        let found = orphans(&repo, &wt_base).unwrap();
        assert!(
            found.iter().any(|p| p.ends_with("ghost-dir")),
            "ghost-dir should be reported as orphan; got: {:?}",
            found
        );
    }

    /// orphans() does not list a legitimately registered worktree.
    #[test]
    fn orphans_excludes_registered_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);

        let wt_base = tmp.path().join("worktrees");
        fs::create_dir_all(&wt_base).unwrap();
        let wt_path = wt_base.join("registered");
        git(
            &repo,
            &[
                "worktree",
                "add",
                wt_path.to_str().unwrap(),
                "-b",
                "feat/reg",
            ],
        )
        .unwrap();

        let found = orphans(&repo, &wt_base).unwrap();
        assert!(
            !found.iter().any(|p| p.ends_with("registered")),
            "registered worktree should not be listed as orphan; got: {:?}",
            found
        );
    }

    /// orphans() must ignore a directory under a *shared* worktree_base that
    /// is actually a live worktree of a completely different repository.
    /// Regression for the bug where a shared worktree_base (e.g.
    /// `/mnt/c/tmp/aegis-worktrees` used by both `harness` and an unrelated
    /// `ai-aegis` checkout) caused condukt to misreport the other project's
    /// legitimate worktrees as orphans belonging to `repo`.
    #[test]
    fn orphans_ignores_other_repos_worktree() {
        let tmp = tempfile::tempdir().unwrap();

        // `repo` is the repo condukt is running orphan-detection for.
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);

        // `other_repo` is a totally unrelated repository that happens to
        // park its worktrees in the same shared base dir.
        let other_repo = tmp.path().join("other_repo");
        fs::create_dir_all(&other_repo).unwrap();
        init_repo(&other_repo);

        let wt_base = tmp.path().join("worktrees");
        fs::create_dir_all(&wt_base).unwrap();

        // A live, legitimate worktree of `other_repo`, sitting in the base
        // dir shared with `repo`.
        let other_wt_path = wt_base.join("other-repos-worktree");
        git(
            &other_repo,
            &[
                "worktree",
                "add",
                other_wt_path.to_str().unwrap(),
                "-b",
                "feat/other",
            ],
        )
        .unwrap();

        let found = orphans(&repo, &wt_base).unwrap();
        assert!(
            !found.iter().any(|p| p.ends_with("other-repos-worktree")),
            "a live worktree belonging to a different repo must not be reported as an orphan of `repo`; got: {:?}",
            found
        );
    }

    /// orphans() must still flag a directory that IS a worktree of `repo`
    /// itself but that git no longer has registered (e.g. because
    /// `git worktree remove` didn't clean it up, leaving a stale `.git`
    /// pointer file behind pointing back into `repo`'s own `.git/worktrees`).
    /// This is the real-world case the shared-worktree_base fix must not
    /// regress: only OTHER repos' worktrees should be ignored, not `repo`'s
    /// own stale ones.
    #[test]
    fn orphans_detects_stale_worktree_of_own_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);

        let wt_base = tmp.path().join("worktrees");
        fs::create_dir_all(&wt_base).unwrap();
        let wt_path = wt_base.join("stale");
        git(
            &repo,
            &[
                "worktree",
                "add",
                wt_path.to_str().unwrap(),
                "-b",
                "feat/stale",
            ],
        )
        .unwrap();

        // Simulate an incomplete `git worktree remove`: git no longer lists
        // it (drop the internal `.git/worktrees/stale` admin dir directly,
        // the way a partial/failed removal can leave things), but the
        // worktree dir and its `.git` pointer file (still pointing back into
        // `repo`) are left behind on disk.
        let admin_dir = repo.join(".git").join("worktrees").join("stale");
        fs::remove_dir_all(&admin_dir).unwrap();

        // Confirm git itself no longer considers it registered.
        let registered = list(&repo).unwrap();
        assert!(
            !registered.iter().any(|(p, _)| p.ends_with("stale")),
            "git should no longer register the stale worktree after its admin dir is removed"
        );

        let found = orphans(&repo, &wt_base).unwrap();
        assert!(
            found.iter().any(|p| p.ends_with("stale")),
            "a stale, unregistered worktree that still belongs to `repo` must be reported as orphan; got: {:?}",
            found
        );
    }

    // ── git subprocess timeout ──────────────────────────────────────────────
    //
    // These tests replace the `git` binary the module shells out to (via a
    // `PATH` override) with a fake hanging script, then drive `git()` through
    // its real code path. Without the `wait_timeout` bound these would block
    // for the test's `sleep` duration (or forever for a truly stuck process);
    // with the bound, `git()` must return a timeout error promptly and not
    // leave the child process running.

    #[cfg(unix)]
    fn write_hanging_fake_git(dir: &Path, sleep_secs: u64) -> PathBuf {
        let path = dir.join("fake-git-hang.sh");
        std::fs::write(
            &path,
            format!("#!/bin/sh\nsleep {sleep_secs}\necho fake-git-output\n"),
        )
        .expect("write fake git");
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod");
        path
    }

    /// A `git` subprocess that hangs well past the configured timeout must
    /// not block the caller forever: it must return a timeout error promptly
    /// (a stand-in for a real hung git — e.g. lock contention or a stuck
    /// credential prompt — that would otherwise wedge a condukt run).
    ///
    /// Drives `run_git_bounded_with` (the exact spawn/wait_timeout/kill code
    /// path `git()` and `git_try()` both use, via `run_git_bounded`) pointed
    /// at a fake script with a short local timeout override, rather than
    /// mutating process-global `PATH` (which would race with the many other
    /// tests in this module that shell out to the real `git` concurrently
    /// under `cargo test`'s multi-threaded runner) or waiting out the full
    /// production `GIT_TIMEOUT` (45s).
    #[cfg(unix)]
    #[test]
    fn git_hung_subprocess_times_out_instead_of_hanging_forever() {
        let tmp = tempfile::tempdir().unwrap();
        // Sleep far longer than the short local timeout below; the timeout
        // must cut this short well before it would ever complete on its own.
        let fake_git = write_hanging_fake_git(tmp.path(), 30);

        let cwd = tmp.path().join("cwd");
        fs::create_dir_all(&cwd).unwrap();

        let short_timeout = Duration::from_millis(300);
        let start = std::time::Instant::now();
        let out = run_git_bounded_with(
            &fake_git,
            &cwd,
            &["rev-parse", "--show-toplevel"],
            short_timeout,
        )
        .expect("run_git_bounded_with itself should not error on timeout");
        let elapsed = start.elapsed();

        assert!(
            out.timed_out,
            "a hung git subprocess must be reported as timed_out, not fabricate success"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "run_git_bounded_with must return promptly on timeout (local override 300ms; \
             production GIT_TIMEOUT is 45s), took {elapsed:?}"
        );
    }

    /// A fast (non-hung) git invocation still returns its real output and is
    /// not spuriously treated as timed out.
    #[cfg(unix)]
    #[test]
    fn git_fast_subprocess_returns_output_without_timing_out() {
        let tmp = tempfile::tempdir().unwrap();
        let fake_git = write_hanging_fake_git(tmp.path(), 0);
        let cwd = tmp.path().join("cwd");
        fs::create_dir_all(&cwd).unwrap();

        let out = run_git_bounded_with(&fake_git, &cwd, &["status"], Duration::from_secs(3))
            .expect("bounded run itself ok");
        assert!(
            !out.timed_out,
            "a fast subprocess must not be treated as timed out"
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "fake-git-output"
        );
    }

    /// The public `git()` wrapper's error-formatting step must turn a
    /// timed-out `GitOutput` into a named timeout error message (not a
    /// bare/empty error), so callers' `.with_context()` chains stay
    /// debuggable. Drives the real `git_output_to_result` formatting step
    /// with a real timed-out `GitOutput` (produced via the fake hanging
    /// binary + a short local timeout), rather than only asserting on a
    /// hand-formatted string.
    #[cfg(unix)]
    #[test]
    fn git_public_wrapper_reports_named_timeout_error() {
        let tmp = tempfile::tempdir().unwrap();
        let fake_git = write_hanging_fake_git(tmp.path(), 30);
        let cwd = tmp.path().join("cwd");
        fs::create_dir_all(&cwd).unwrap();

        let short_timeout = Duration::from_millis(300);
        let args: &[&str] = &["status"];
        let out = run_git_bounded_with(&fake_git, &cwd, args, short_timeout)
            .expect("bounded run itself should not error");
        assert!(out.timed_out, "expected the fake git call to time out");

        let result = git_output_to_result(out, &cwd, args, short_timeout);
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("timed out") || err.contains("timeout"),
            "error should name the timeout as the cause, got: {err}"
        );
        assert!(
            err.contains("status"),
            "error should name the git subcommand, got: {err}"
        );
    }

    // ── path canonicalization before the "outside the repo" check ──────────

    /// `canonicalize_prefix` on an existing path is equivalent to plain
    /// `canonicalize()`.
    #[test]
    fn canonicalize_prefix_existing_path_matches_canonicalize() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("sub");
        fs::create_dir_all(&sub).unwrap();
        assert_eq!(canonicalize_prefix(&sub), sub.canonicalize().unwrap());
    }

    /// `canonicalize_prefix` on a path whose leaf components don't exist yet
    /// (the common case for a not-yet-created worktree dir) resolves the
    /// existing ancestor and rejoins the non-existent suffix, rather than
    /// returning the raw uncanonicalized path.
    #[test]
    fn canonicalize_prefix_nonexistent_leaf_resolves_existing_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("base");
        fs::create_dir_all(&base).unwrap();
        let target = base.join("not-yet-created").join("topic");
        assert!(!target.exists());

        let resolved = canonicalize_prefix(&target);
        let expected = base.canonicalize().unwrap().join("not-yet-created/topic");
        assert_eq!(resolved, expected);
    }

    /// `create()` must refuse a worktree whose canonical path resolves inside
    /// the repo even when the *raw* `worktree_base` path used to construct it
    /// is not itself in canonical form (e.g. contains a symlink hop, or an
    /// extra `.`/redundant separator that `Path::starts_with` would treat as
    /// a different prefix than the repo's canonical root). This exercises the
    /// worktree.rs:121-124-area canonicalize fix: comparing a canonical
    /// `path` against `repo_canon`, not the raw `path` against `repo_canon`.
    #[cfg(unix)]
    #[test]
    fn create_rejects_worktree_inside_repo_via_noncanonical_base() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);

        // A symlink that points *into* the repo. Using it as `worktree_base`
        // means the raw (non-canonical) path looks like it's outside the
        // repo (different leading component), but its canonical form
        // resolves inside the repo.
        let link = tmp.path().join("repo-alias");
        std::os::unix::fs::symlink(&repo, &link).unwrap();

        let err = create(&repo, &link, "nested-topic", "condukt/x").unwrap_err();
        assert!(
            err.to_string().contains("inside the repo"),
            "worktree_base reached via a symlink into the repo must still be \
             rejected as 'inside the repo', got: {err}"
        );
    }

    // ── prune stale registrations before the branch-checked-out gate ────────

    /// `create()` must PRUNE a stale (dir-removed, admin-entry-lingering)
    /// worktree registration AND force-delete the lingering branch ref before
    /// re-cutting, so the same condukt branch can be re-cut after its worktree
    /// dir was dropped out-of-band. Two failure modes are encoded here:
    ///   (a) Without prune-first, git still reports the branch as "checked out
    ///       in another worktree" and `create()` false-blocks the re-creation.
    ///   (b) With prune-only OR re-attach (the rejected fix), the re-cut branch
    ///       is NOT pristine: the dropped attempt's commit survives on the ref
    ///       and is silently inherited. The re-cut MUST instead be branched
    ///       fresh off base HEAD (the `git branch -D` + `-b` fix).
    #[test]
    fn create_prunes_stale_registration_and_recreates_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);

        // Capture the base HEAD that every fresh re-cut must branch from.
        let base_head = git(&repo, &["rev-parse", "HEAD"]).unwrap();

        let wt_base = tmp.path().join("worktrees");
        fs::create_dir_all(&wt_base).unwrap();
        let branch = "condukt/recut";

        // 1. Create a worktree for `branch` via the real create() path.
        let wt_path = create(&repo, &wt_base, "recut", branch).unwrap();
        assert!(wt_path.exists(), "worktree dir should exist after create");

        // 1b. Seed the (soon-to-be-dropped) attempt with a commit on `branch`.
        //     This stands in for a crashed/abandoned worker's committed work
        //     that survives on the ref. Its SHA must NOT reappear on the re-cut.
        fs::write(wt_path.join("dropped-attempt.txt"), "stale work\n").unwrap();
        git(&wt_path, &["add", "dropped-attempt.txt"]).unwrap();
        git(&wt_path, &["commit", "-m", "dropped attempt commit"]).unwrap();
        let dropped_sha = git(&wt_path, &["rev-parse", "HEAD"]).unwrap();
        assert_ne!(
            dropped_sha, base_head,
            "the dropped attempt must have advanced the branch past base HEAD"
        );

        // 2. Remove the worktree DIRECTORY out-of-band, leaving git's stale
        //    admin registration (and the branch ref, with its extra commit)
        //    behind — git still thinks the branch is checked out there.
        fs::remove_dir_all(&wt_path).unwrap();
        assert!(
            branch_checked_out(&repo, branch).unwrap(),
            "precondition: git still reports a stale registration for the branch"
        );

        // 3. Re-create for the same branch. With prune + force-delete this
        //    SUCCEEDS and yields a PRISTINE branch; against the old code it
        //    false-blocks (a) or re-attaches the stale ref (b).
        let recreated = create(&repo, &wt_base, "recut", branch)
            .expect("re-creating the branch must succeed after pruning the stale registration");
        assert!(
            recreated.exists(),
            "re-created worktree dir should exist: {}",
            recreated.display()
        );

        // 4. PRISTINE assertion (the design fix): the re-cut branch must point
        //    at base HEAD and must NOT contain the dropped attempt's commit.
        //    A prune-only or re-attach implementation fails this: the re-cut
        //    branch would still carry `dropped_sha`.
        let recut_head = git(&recreated, &["rev-parse", "HEAD"]).unwrap();
        assert_eq!(
            recut_head, base_head,
            "re-cut branch must be branched fresh off base HEAD, not inherit the dropped ref"
        );
        assert_ne!(
            recut_head, dropped_sha,
            "re-cut branch must NOT point at the dropped attempt's commit"
        );
        // The dropped attempt's file must not be present in the fresh worktree.
        assert!(
            !recreated.join("dropped-attempt.txt").exists(),
            "the dropped attempt's committed file must not resurface on a pristine re-cut"
        );
        // And the dropped commit must not be an ancestor of the re-cut branch.
        let (contains_dropped, _, _) = git_try(
            &repo,
            &["merge-base", "--is-ancestor", &dropped_sha, branch],
        )
        .unwrap();
        assert!(
            !contains_dropped,
            "the dropped attempt's commit must not be reachable from the re-cut branch"
        );
    }

    /// The prune-first step must NOT weaken a legitimate rejection: a branch
    /// genuinely checked out in a LIVE worktree survives `git worktree prune`,
    /// so `create()` still refuses to re-cut it (one dir = one branch).
    #[test]
    fn create_still_rejects_branch_live_in_another_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);

        let wt_base = tmp.path().join("worktrees");
        fs::create_dir_all(&wt_base).unwrap();
        let branch = "condukt/live";

        // A LIVE worktree for `branch` (dir stays on disk).
        create(&repo, &wt_base, "live", branch).unwrap();

        // Re-cutting the same branch into a different topic must still be
        // rejected — prune leaves a live registration untouched.
        let err = create(&repo, &wt_base, "live2", branch).unwrap_err();
        assert!(
            err.to_string().contains("already checked out"),
            "a branch live in another worktree must still be rejected, got: {err}"
        );
    }
}
