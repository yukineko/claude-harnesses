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
    // `trim_end`, NOT `trim`: git's own output (e.g. `status --porcelain`)
    // can carry a semantically meaningful LEADING space on its first line —
    // the "not staged" status column of the first entry. A blanket `.trim()`
    // eats that byte (it sits at position 0 of the whole blob), which made
    // `repo_commit::staged_paths` misread an ordinary unstaged edit as
    // staged. Only the trailing newline(s) `git` always appends need
    // stripping.
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
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

/// Validate a run id used as a cross-session NAMESPACE segment. It is spliced
/// into both a worktree topic (one path component) and a branch ref, so it must
/// satisfy the *stricter* of the two: the [`validate_topic`] character class,
/// with no path separators. Run ids are `run-%Y%m%d-%H%M%S` by default but can
/// be supplied by the caller, so they are untrusted input.
fn validate_run_ns(run: &str) -> Result<()> {
    if run.is_empty() {
        bail!("run namespace must not be empty");
    }
    if run.starts_with('-') || run.starts_with('.') {
        bail!("run namespace {run:?} must not start with '-' or '.'");
    }
    if run.contains("..") {
        bail!("run namespace {run:?} must not contain '..'");
    }
    if !run
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        bail!("run namespace {run:?} may only contain [A-Za-z0-9._-] (no path separators)");
    }
    Ok(())
}

/// The run-scoped worktree topic for `topic`.
///
/// Joined with `-`, NOT `/`, on purpose — two reasons, both load-bearing:
///
/// 1. [`validate_topic`] rejects path separators (a topic is a *single*
///    component appended to `worktree_base`); that rejection is a traversal
///    guard we do not want to relax, so the namespace is folded into the same
///    single component instead.
/// 2. [`orphans`] reads only the TOP level of `worktree_base`. A nested
///    `<base>/<run>/<t1>` layout would make the intermediate `<base>/<run>` dir
///    look like a `.git`-less, unattributable directory — i.e. it would be
///    reported as condukt debris and swept.
fn namespaced_topic(run: &str, topic: &str) -> String {
    format!("{run}-{topic}")
}

/// The run-scoped branch ref for `branch`: the run id is inserted as its own
/// path segment immediately before the final segment, so `condukt/t1` under run
/// `run-A` becomes `condukt/run-A/t1`. Branch refs may contain `/` (see
/// [`validate_branch`]), which keeps the refs grouped and greppable
/// (`git branch --list 'condukt/run-A/*'`) and — critically — makes two runs
/// emitting the same task id aim at DIFFERENT refs, so neither can be the
/// "stale leftover" that [`create`] force-deletes.
///
/// This is additive with respect to legacy refs: an old un-namespaced
/// `refs/heads/condukt/t1` is a ref *file* under `refs/heads/condukt/`, while
/// the new ref lives under `refs/heads/condukt/<run>/`, so the two never
/// collide and the old one is never targeted by the new naming.
fn namespaced_branch(run: &str, branch: &str) -> String {
    match branch.rsplit_once('/') {
        Some((prefix, last)) => format!("{prefix}/{run}/{last}"),
        None => format!("{run}/{branch}"),
    }
}

/// Cross-session-namespaced [`create`]: cut the worktree for `topic`/`branch`
/// under the namespace of run `run`.
///
/// Task ids (`t1`, `t2`, …) are per-run and NOT comparable across runs
/// (`crate::claim` module docs), while `worktree_base` is machine-global. Two
/// concurrent sessions that both emit `t1` therefore used to aim at the exact
/// same dir and the exact same branch ref, which meant a hard bail at best and,
/// at worst, `create`'s stale-ref `git branch -D` silently destroying a peer
/// session's unmerged commits. Namespacing by run id removes the shared name,
/// so the force-delete can only ever target this run's own leftover ref.
///
/// `run` is `None` for legacy/un-namespaced callers, which get exactly the old
/// [`create`] behavior (byte-identical paths and refs) — worktrees and branches
/// created by an older condukt stay addressable and are never re-targeted.
pub fn create_namespaced(
    repo: &Path,
    worktree_base: &Path,
    run: Option<&str>,
    topic: &str,
    branch: &str,
) -> Result<PathBuf> {
    match run {
        None => create(repo, worktree_base, topic, branch),
        Some(run) => {
            validate_run_ns(run)?;
            // Validate the *inputs* too, so a bad topic/branch is reported
            // against what the caller passed rather than against the spliced
            // form (create() re-validates the spliced result regardless).
            validate_topic(topic)?;
            validate_branch(branch)?;
            create(
                repo,
                worktree_base,
                &namespaced_topic(run, topic),
                &namespaced_branch(run, branch),
            )
        }
    }
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
    //
    // Cannot-acquire REFUSES (`?`). The lock does not merely guard the prune: it
    // spans the `branch -D` of a stale ref and the `worktree add` below, so
    // proceeding unlocked could force-delete a ref a peer's merge is landing, or
    // race another run's worktree-admin mutation. Refusing is safe here because
    // nothing has been mutated yet — the caller gets an error and can retry,
    // which beats a half-created worktree nobody is tracking.
    let _repo_lock = lock::acquire_repo_primary_loaded(repo)?;
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

/// The outcome of [`merge`]. A blocked merge (a mid-flight runtime-overlap HOLD
/// or a real 3-way conflict) is NOT a hard error — it is a hold-for-review
/// (design 625aa170): the merge is skipped/recorded and the caller reports it
/// and exits 0. Only a genuine git failure (a failed checkout, etc.) is an
/// `Err`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// The branch was merged into the default branch.
    Merged,
    /// A mid-flight runtime overlap (decision A) held this branch BEFORE the
    /// merge was attempted; carries the open hold's `conflict_id`. Resolve it
    /// (`overwatch resolve-merge-conflict` + `condukt worktree resolve-merge`)
    /// then retry.
    Held(String),
    /// A real 3-way conflict was detected in the trial merge; a
    /// `MergeConflictEntry` was recorded to the consensus review surface (both
    /// diffs + conflicted files) and the merge was aborted. Carries the
    /// recorded `conflict_id`.
    Conflict(String),
}

/// Is a runtime-overlap merge-hold placed by `run_id` still AUTHORITATIVE — is
/// its placer run still LIVE, per condukt's OWN claim/heartbeat registry? A hold
/// only blocks the merge while its holder run is alive: an overlap held by a run
/// that has since crashed/been abandoned is stale and must NOT block a LIVE task
/// that merely REUSES the same `condukt/<task.id>` branch name in a later run
/// (the bug: `finalize_landed_branch` only clears holds on a branch that LANDS,
/// so a never-landed abandoned branch leaves a stale OPEN hold that spuriously
/// blocks the reused-branch task forever).
///
/// Liveness is read from condukt's OWN registry — [`crate::claim::run_liveness`],
/// the claim/heartbeat TTL machinery keyed on the SAME condukt run-id space the
/// hold's `run_id` lives in (a live run holds file/task claims under its run id
/// and heart-beats them). It is deliberately NOT read from overwatch leases:
/// condukt never begins an overwatch lease and `OVERWATCH_RUN_ID` is unset, so a
/// lease-based liveness check runs against a DISJOINT id space, matches nothing,
/// and reads EVERY hold as dead — silently disabling this gate in production.
/// Staying inside condukt's own id space is the whole point.
///
/// **Fail CLOSED on ambiguity.** The hold keeps blocking unless we have POSITIVE
/// evidence its placer run is dead: only a CLEAN registry read that finds no
/// fresh claim ([`crate::claim::RunLiveness::Dead`]) drops it. `Live` obviously
/// keeps it, and `Undetermined` — a present-but-unreadable/corrupt registry, or
/// an UNATTRIBUTED hold (empty `run_id`, no placer to test) — ALSO keeps it.
/// Letting the merge proceed on "could not determine liveness" is precisely the
/// silent fail-open this gate exists to prevent (the reverted disjoint-id bug's
/// failure class — an empty read making every hold look dead — re-entering via
/// registry absence instead of id mismatch).
///
/// **Known residual (tracked, not a cannot-determine fail-open).** Liveness is
/// keyed purely on the claim registry, so a run that placed a hold but holds NO
/// claim under `run_id` — e.g. a task whose decomposition `touched_files` was
/// empty (claims key off declared `touched_files`, while the hold is placed off
/// the actual git diff), or a run that already released its claims at the
/// `verified` transition while an open hold lingers — reads as `Dead` and its
/// hold drops. This is a positive-evidence heuristic (a CLEAN read finding no
/// live claim), the SAME basis the reaper uses — categorically different from the
/// `Undetermined` fail-closed path above. The proper closure is a second
/// positive signal (is `run_id` a currently-active, non-terminal run in
/// `state::RunState`?); tracked as a backlog follow-up rather than coupling
/// run-state into this gate under the current fix.
fn hold_placed_by_live_run(cfg: &Config, repo: &Path, run_id: &str, now: i64) -> bool {
    match crate::claim::run_liveness(cfg, repo, run_id, now) {
        crate::claim::RunLiveness::Dead => false,
        crate::claim::RunLiveness::Live | crate::claim::RunLiveness::Undetermined => true,
    }
}

/// Look up an OPEN runtime-overlap merge-hold for `branch` in the overwatch
/// review surface (decision A). Returns the hold's `conflict_id` when the branch
/// is held BY A STILL-LIVE holder run. Fail-soft: any read error / absent store
/// degrades to "no hold" (never blocks a merge on a compute error).
///
/// Authority is filtered by holder-run LIVENESS, not branch-name match alone
/// (see [`hold_placed_by_live_run`]): a stale hold from a DEAD condukt run does
/// not block a task that reuses the branch name, while a hold from a LIVE run
/// still blocks.
fn open_runtime_overlap_hold(cfg: &Config, repo: &Path, branch: &str) -> Option<String> {
    let open = overwatch::store::open_merge_conflicts(repo).ok()?;
    let now = crate::state::now_secs();
    open.into_iter()
        .find(|e| {
            e.branch == branch
                && matches!(
                    e.origin,
                    overwatch::merge_conflict::ConflictOrigin::RuntimeOverlap
                )
                && hold_placed_by_live_run(cfg, repo, &e.run_id, now)
        })
        .map(|e| e.conflict_id)
}

/// Parse the unique conflicted paths out of `git ls-files --unmerged` output
/// (lines shaped `<mode> <sha> <stage>\t<path>`, one per stage). Order-preserving,
/// de-duplicated.
fn parse_unmerged_paths(unmerged: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in unmerged.lines() {
        if let Some((_, path)) = line.split_once('\t') {
            let p = path.trim().to_string();
            if !p.is_empty() && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// The frozen three-dot merge-base SHA of `default_branch` and `branch` (the
/// point both diffs are taken against). Falls back to `default_branch` when the
/// merge-base can't be resolved.
fn merge_base_sha(repo: &Path, default_branch: &str, branch: &str) -> String {
    git(repo, &["merge-base", default_branch, branch])
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_branch.to_string())
}

/// Best-effort `git diff <from>...<to>` (three-dot), empty on failure.
fn git_diff_range(repo: &Path, from: &str, to: &str) -> String {
    git(repo, &["diff", &format!("{from}...{to}")]).unwrap_or_default()
}

/// Merge `branch` into the configured default branch. Verifies the repo is
/// actually on the default branch first (verify -> act).
///
/// Gate (design 625aa170):
/// 1. **Pre-merge hold (decision A)**: if a mid-flight runtime overlap has HELD
///    this branch for review, skip the merge and return [`MergeOutcome::Held`].
/// 2. **Trial merge (no-commit)**: on a real 3-way conflict, CAPTURE a
///    `MergeConflictEntry` (conflicted files + both byte-bounded diffs) to the
///    consensus review surface, abort the trial, and return
///    [`MergeOutcome::Conflict`] — the merge still stops (block PRESERVED) but
///    it is now visible/resolvable instead of a silent local-only degrade, and
///    it is a hold-for-review (exit 0), not a hard error.
/// 3. **Clean**: abort the trial and perform the real merge → [`MergeOutcome::Merged`].
pub fn merge(
    cfg: &Config,
    repo: &Path,
    branch: &str,
    default_branch: &str,
) -> Result<MergeOutcome> {
    // Serialize with every other primary-repo default-branch mutator (another
    // condukt run's merge / main-tree commit / `git worktree prune`) on ONE
    // repo-scoped lock, so two runs in the same repo never race on the default
    // branch. Held for the entire checkout+merge critical section and released
    // on drop.
    //
    // Cannot-acquire REFUSES (`?`): this path checks out `default_branch` in the
    // SHARED primary working tree and commits a merge onto it. Unlocked, it can
    // interleave with a peer's `repo commit` staging into the same index, with
    // another run's merge, or with a `worktree prune`. An unheld lock is
    // cannot-determine and resolves to the restrictive side. Refusing costs
    // nothing here: it happens before any git mutation, so the merge is simply
    // retryable.
    let _repo_lock = lock::acquire_repo_primary(cfg, repo)?;

    // ── Pre-merge hold gate (decision A) ─────────────────────────────────────
    // A detected mid-flight actual-diff overlap HOLDS this branch for review.
    // Do not merge until it is resolved (the resolution clears the hold).
    if let Some(conflict_id) = open_runtime_overlap_hold(cfg, repo, branch) {
        return Ok(MergeOutcome::Held(conflict_id));
    }

    git(repo, &["checkout", default_branch])
        .with_context(|| format!("could not checkout {default_branch} before merge"))?;
    let current = git(repo, &["branch", "--show-current"])?;
    if current != default_branch {
        bail!("expected to be on '{default_branch}' but on '{current}'; aborting merge");
    }

    // ── Pre-flight: trial merge (no-commit) to detect conflicts ──────────────
    // `git merge --no-commit --no-ff` either succeeds with a staged merge or
    // fails immediately when conflicts exist. Capture the conflicted paths from
    // the index BEFORE aborting (works whether the trial exited non-zero or left
    // CONFLICT markers with a zero exit).
    let (trial_ok, _, _trial_stderr) = git_try(repo, &["merge", "--no-commit", "--no-ff", branch])?;
    let unmerged = git(repo, &["ls-files", "--unmerged"]).unwrap_or_default();
    let mut conflicted = parse_unmerged_paths(&unmerged);

    if !trial_ok || !conflicted.is_empty() {
        // Capture BEFORE the abort: conflicted files + both sides' diffs (frozen
        // merge-base, byte-bounded). Recorded to the consensus review surface so
        // the conflict is visible/resolvable instead of a silent local degrade.
        let base = merge_base_sha(repo, default_branch, branch);
        if conflicted.is_empty() {
            // Trial failed before staging any index entries — fall back to the
            // symmetric name-only diff so the entry still names the files.
            conflicted = git(
                repo,
                &[
                    "diff",
                    "--name-only",
                    &format!("{default_branch}...{branch}"),
                ],
            )
            .map(|s| {
                s.lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        }
        let conflict_id = format!("{branch}/merge-conflict");
        let entry = overwatch::merge_conflict::MergeConflictEntry {
            conflict_id: conflict_id.clone(),
            origin: overwatch::merge_conflict::ConflictOrigin::MergeConflict,
            run_id: String::new(),
            branch: branch.to_string(),
            default_branch: default_branch.to_string(),
            base_ref: base.clone(),
            conflicted_files: conflicted,
            diff_ours: overwatch::merge_conflict::truncate_diff(
                &git_diff_range(repo, &base, default_branch),
                overwatch::merge_conflict::DIFF_BYTE_CAP,
            ),
            diff_theirs: overwatch::merge_conflict::truncate_diff(
                &git_diff_range(repo, &base, branch),
                overwatch::merge_conflict::DIFF_BYTE_CAP,
            ),
            ts: overwatch::store::now(),
        };
        // Idempotent per conflict_id. Fail-soft on the write (recording must
        // never break the turn); the abort below always runs.
        if let Err(e) = overwatch::store::append_merge_conflict(repo, &entry) {
            eprintln!("condukt: could not record merge conflict (continuing): {e}");
        }
        let _ = git_try(repo, &["merge", "--abort"]);
        return Ok(MergeOutcome::Conflict(conflict_id));
    }

    // No conflicts found in trial — abort the staged trial and do the real merge.
    git_try(repo, &["merge", "--abort"])
        .with_context(|| "could not abort trial merge before real merge")?;

    git(repo, &["merge", "--no-edit", branch])
        .with_context(|| format!("merge of {branch} into {default_branch} failed"))?;
    finalize_landed_branch(repo, branch);
    Ok(MergeOutcome::Merged)
}

/// On-merge cleanup (design 625aa170 decision A — the cleanup half). Once
/// `branch` has LANDED, take it out of the mid-flight overlap-detection set and
/// tidy the cross-run registry. Called from BOTH successful merge paths
/// ([`merge`]'s `Merged` outcome and [`resolve_merge`]'s reconciled outcomes),
/// which is why it lives in `worktree::merge` rather than each condukt `main.rs`
/// caller — every merge caller (`run_pr` Poll, `run_worktree` Merge) routes
/// through here, so the wiring is DRY and unit-testable.
///
/// Fail-soft by contract: the merge already succeeded, so a cleanup error must
/// NEVER surface as an `Err` (that would report a green merge as failed). Every
/// step logs-and-continues.
///
/// 1. mark every in-flight changeset for `branch` merged so it leaves the
///    detection set (closes finding #1: the next sequential task that touches a
///    common file is no longer spuriously Held);
/// 2. clear any OPEN runtime-overlap hold recorded against `branch` (defensive
///    against a stale hold from a REUSED branch name blocking a later run);
/// 3. opportunistically prune merged/stale changesets so the cross-run registry
///    stays bounded (finding #4).
fn finalize_landed_branch(repo: &Path, branch: &str) {
    if let Err(e) = overwatch::store::mark_branch_merged(repo, branch) {
        eprintln!("condukt: could not mark landed branch '{branch}' merged (continuing): {e}");
    }
    let now = overwatch::store::now();
    if let Err(e) = overwatch::store::clear_runtime_overlap_holds(repo, branch, now) {
        eprintln!(
            "condukt: could not clear runtime-overlap holds for '{branch}' (continuing): {e}"
        );
    }
    if let Err(e) = overwatch::store::prune_stale_changesets(repo, now) {
        eprintln!("condukt: could not prune stale changesets (continuing): {e}");
    }
}

/// The outcome of [`resolve_merge`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveOutcome {
    /// The recorded resolution was reconciled (an `Ours`/`Theirs` strategy was
    /// applied, or the branch already merged cleanly) and the merge is done.
    Reconciled(overwatch::merge_conflict::ResolveChoice),
    /// A `Manual` resolution left conflict markers materialized in the working
    /// tree for a human/worker to resolve and commit (carries the paths). The
    /// merge completes when they commit the in-progress merge.
    ManualPending(Vec<String>),
}

/// Reconciliation driver (design 625aa170 decision B): apply a RECORDED
/// resolution to a blocked merge and complete it. Reads the
/// [`overwatch::merge_conflict::MergeConflictEntry`] (for the branch pair) and
/// its [`overwatch::merge_conflict::MergeConflictResolution`] (for the chosen
/// side) from the consensus review surface, then under the repo-primary lock:
///
/// - **Ours** → `git merge -s ours` (keep the default branch's version; the
///   branch is recorded as merged so it leaves the in-flight set).
/// - **Theirs** → `git merge -X theirs` (take the feature branch's version on
///   conflicting hunks).
/// - **Manual** → materialize the merge with conflict markers and stop
///   ([`ResolveOutcome::ManualPending`]); a human/worker resolves + commits.
///
/// Requires a resolution to exist first (written by `overwatch
/// resolve-merge-conflict`) — recording the resolution is ALSO what clears the
/// runtime-overlap HOLD (it leaves the OPEN set), so a re-run of [`merge`] is
/// no longer blocked. Never auto-picks a side on its own: the choice always
/// comes from the recorded human/policy decision.
pub fn resolve_merge(cfg: &Config, repo: &Path, conflict_id: &str) -> Result<ResolveOutcome> {
    use overwatch::merge_conflict::ResolveChoice;

    // Serialize with every other default-branch mutator (same lock `merge`
    // holds), and REFUSE on cannot-acquire (`?`) for the same reason: this path
    // checks out `default_branch` and completes a merge onto it in the shared
    // primary working tree. Taken FIRST, before the store reads below, so a
    // degraded lock can never be masked by a later "no such conflict" error.
    let _repo_lock = lock::acquire_repo_primary(cfg, repo)?;

    // The entry names the branch pair. Read from the full stream (the entry may
    // already be resolved, i.e. out of the OPEN set) and take the latest match.
    let entries = overwatch::store::read_merge_conflicts(repo)
        .with_context(|| "could not read merge-conflict entries")?;
    let entry = entries
        .into_iter()
        .rev()
        .find(|e| e.conflict_id == conflict_id)
        .ok_or_else(|| anyhow!("no merge-conflict entry with id '{conflict_id}'"))?;

    // The recorded decision (ours/theirs/manual). Without one there is nothing
    // to reconcile — the human/policy must decide first.
    let resolution = overwatch::store::find_merge_conflict_resolution(repo, conflict_id)
        .with_context(|| "could not read merge-conflict resolutions")?
        .ok_or_else(|| {
            anyhow!(
                "conflict '{conflict_id}' has no recorded resolution yet; run \
                 `overwatch resolve-merge-conflict --id {conflict_id} --choose ours|theirs|manual` first"
            )
        })?;

    let branch = &entry.branch;
    let default_branch = &entry.default_branch;

    git(repo, &["checkout", default_branch])
        .with_context(|| format!("could not checkout {default_branch} before resolve-merge"))?;
    let current = git(repo, &["branch", "--show-current"])?;
    if current != *default_branch {
        bail!("expected to be on '{default_branch}' but on '{current}'; aborting resolve-merge");
    }

    match resolution.choice {
        ResolveChoice::Ours => {
            // Keep our side; still record the merge so the branch is considered
            // merged and leaves the in-flight set.
            git(repo, &["merge", "-s", "ours", "--no-edit", branch]).with_context(|| {
                format!("`merge -s ours` of {branch} into {default_branch} failed")
            })?;
            finalize_landed_branch(repo, branch);
            Ok(ResolveOutcome::Reconciled(ResolveChoice::Ours))
        }
        ResolveChoice::Theirs => {
            // Favor the branch on conflicting hunks.
            git(repo, &["merge", "-X", "theirs", "--no-edit", branch]).with_context(|| {
                format!("`merge -X theirs` of {branch} into {default_branch} failed")
            })?;
            finalize_landed_branch(repo, branch);
            Ok(ResolveOutcome::Reconciled(ResolveChoice::Theirs))
        }
        ResolveChoice::Manual => {
            // Materialize the merge. If the branch merges cleanly (e.g. a worker
            // already resolved it on the branch), commit and report Reconciled;
            // otherwise leave the markers in place for a human/worker to finish.
            let (ok, _, _) = git_try(repo, &["merge", "--no-edit", branch])?;
            let unmerged = git(repo, &["ls-files", "--unmerged"]).unwrap_or_default();
            let pending = parse_unmerged_paths(&unmerged);
            if ok && pending.is_empty() {
                finalize_landed_branch(repo, branch);
                Ok(ResolveOutcome::Reconciled(ResolveChoice::Manual))
            } else {
                // Merge left conflict markers materialized in the working tree.
                Ok(ResolveOutcome::ManualPending(pending))
            }
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod worktree_remove_tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // `merge()` now reads/writes the overwatch store, which keys off $HOME.
    // Serialize the HOME swap so a merge test never races another HOME-mutating
    // test and never touches the real `~/.overwatch`.
    static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Run `f` with `$HOME` pinned to `home` (restored afterwards), serialized
    /// against other HOME swaps. Returns `f`'s value.
    fn with_home<R>(home: &Path, f: impl FnOnce() -> R) -> R {
        let _g = HOME_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", home);
        let out = f();
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        out
    }

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
        let (tmp, repo) = init_repo();
        make_branch(&repo, "feat", "feat.txt", "feature content\n");

        let cfg = test_cfg(&repo);
        let home = tmp.path().join("home-clean");
        fs::create_dir_all(&home).unwrap();
        let outcome = with_home(&home, || {
            merge(&cfg, &repo, "feat", "main").expect("clean merge should succeed")
        });
        assert_eq!(
            outcome,
            MergeOutcome::Merged,
            "a clean merge must report Merged"
        );

        // The file should now exist on main
        assert!(repo.join("feat.txt").exists());
    }

    /// A real 3-way conflict is NO LONGER a hard error (design 625aa170): the
    /// merge is HELD for review — `merge()` returns `MergeOutcome::Conflict`,
    /// records a `MergeConflictEntry` (both diffs + the conflicted file) to the
    /// overwatch consensus review surface, and aborts so the repo stays clean.
    #[test]
    fn worktree_merge_conflict_records_entry_and_holds() {
        let (tmp, repo) = init_repo();

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
        let home = tmp.path().join("home-conflict");
        fs::create_dir_all(&home).unwrap();
        let (outcome, open) = with_home(&home, || {
            let outcome = merge(&cfg, &repo, "conflict-branch", "main")
                .expect("a held conflict is a hold-for-review, not a hard error");
            // Read back the recorded entries under the sandboxed HOME.
            let open = overwatch::store::open_merge_conflicts(&repo).unwrap_or_default();
            (outcome, open)
        });

        let conflict_id = match &outcome {
            MergeOutcome::Conflict(id) => id.clone(),
            other => panic!("conflicting merge must report Conflict, got {other:?}"),
        };
        assert_eq!(conflict_id, "conflict-branch/merge-conflict");

        // The conflict was RECORDED to the consensus review surface (open set).
        let entry = open
            .iter()
            .find(|e| e.conflict_id == conflict_id)
            .expect("the conflict must be recorded as an open merge-conflict entry");
        assert_eq!(
            entry.origin,
            overwatch::merge_conflict::ConflictOrigin::MergeConflict
        );
        assert_eq!(entry.branch, "conflict-branch");
        assert_eq!(entry.default_branch, "main");
        assert!(
            entry.conflicted_files.iter().any(|f| f == conflict_file),
            "the entry must name the conflicted file, got {:?}",
            entry.conflicted_files
        );
        assert!(
            !entry.diff_ours.is_empty() && !entry.diff_theirs.is_empty(),
            "the entry must capture BOTH sides' diffs"
        );

        // The repo should be in a clean state (no in-progress merge)
        let merge_head = repo.join(".git").join("MERGE_HEAD");
        assert!(
            !merge_head.exists(),
            "MERGE_HEAD should not exist after aborted pre-flight"
        );
    }

    /// Build a repo with a guaranteed 3-way conflict on `shared.txt` between
    /// `conflict-branch` ("branch version") and `main` ("main version"), and
    /// record a `MergeConflictEntry` for it (origin MergeConflict). Returns
    /// `(tmp, repo, conflict_id)`. Caller runs under a sandboxed HOME.
    fn seed_conflict_repo() -> (TempDir, PathBuf, String) {
        let (tmp, repo) = init_repo();
        let conflict_file = "shared.txt";
        fs::write(repo.join(conflict_file), "line1\nline2\nline3\n").unwrap();
        git(&repo, &["add", "."]).unwrap();
        git(&repo, &["commit", "-m", "add shared file"]).unwrap();

        git(&repo, &["checkout", "-b", "conflict-branch"]).unwrap();
        fs::write(repo.join(conflict_file), "line1\nbranch version\nline3\n").unwrap();
        git(&repo, &["add", "."]).unwrap();
        git(&repo, &["commit", "-m", "branch edit"]).unwrap();

        git(&repo, &["checkout", "main"]).unwrap();
        fs::write(repo.join(conflict_file), "line1\nmain version\nline3\n").unwrap();
        git(&repo, &["add", "."]).unwrap();
        git(&repo, &["commit", "-m", "main edit"]).unwrap();

        let conflict_id = "conflict-branch/merge-conflict".to_string();
        let entry = overwatch::merge_conflict::MergeConflictEntry {
            conflict_id: conflict_id.clone(),
            origin: overwatch::merge_conflict::ConflictOrigin::MergeConflict,
            run_id: String::new(),
            branch: "conflict-branch".to_string(),
            default_branch: "main".to_string(),
            base_ref: merge_base_sha(&repo, "main", "conflict-branch"),
            conflicted_files: vec![conflict_file.to_string()],
            diff_ours: "ours".to_string(),
            diff_theirs: "theirs".to_string(),
            ts: overwatch::store::now(),
        };
        overwatch::store::append_merge_conflict(&repo, &entry).unwrap();
        (tmp, repo, conflict_id)
    }

    /// Record a resolution (`overwatch resolve-merge-conflict` equivalent) for
    /// `conflict_id` under the sandboxed HOME.
    fn record_resolution(
        repo: &Path,
        conflict_id: &str,
        choice: overwatch::merge_conflict::ResolveChoice,
    ) {
        let resolution = overwatch::merge_conflict::MergeConflictResolution {
            conflict_id: conflict_id.to_string(),
            choice,
            decided_by: overwatch::merge_conflict::DecidedBy::Human,
            note: None,
            ts: overwatch::store::now(),
        };
        overwatch::store::append_merge_conflict_resolution(repo, &resolution).unwrap();
    }

    #[test]
    fn resolve_merge_requires_a_recorded_resolution_first() {
        let home = TempDir::new().unwrap();
        with_home(home.path(), || {
            let (_tmp, repo, conflict_id) = seed_conflict_repo();
            let cfg = test_cfg(&repo);
            // No resolution recorded yet → resolve_merge must error, NOT pick a side.
            let err = resolve_merge(&cfg, &repo, &conflict_id)
                .expect_err("resolve-merge with no recorded resolution must error");
            let msg = format!("{err:#}");
            assert!(
                msg.contains("no recorded resolution"),
                "error should explain the missing resolution, got: {msg}"
            );
        });
    }

    #[test]
    fn resolve_merge_ours_completes_keeping_default_content() {
        let home = TempDir::new().unwrap();
        with_home(home.path(), || {
            let (_tmp, repo, conflict_id) = seed_conflict_repo();
            record_resolution(
                &repo,
                &conflict_id,
                overwatch::merge_conflict::ResolveChoice::Ours,
            );
            let cfg = test_cfg(&repo);
            let out =
                resolve_merge(&cfg, &repo, &conflict_id).expect("ours resolution should reconcile");
            assert_eq!(
                out,
                ResolveOutcome::Reconciled(overwatch::merge_conflict::ResolveChoice::Ours)
            );
            // The merge is completed (no in-progress merge) keeping OUR (main) content.
            assert!(!repo.join(".git").join("MERGE_HEAD").exists());
            let content = fs::read_to_string(repo.join("shared.txt")).unwrap();
            assert!(
                content.contains("main version") && !content.contains("branch version"),
                "ours must keep the default branch content, got: {content}"
            );
        });
    }

    #[test]
    fn resolve_merge_theirs_completes_taking_branch_content() {
        let home = TempDir::new().unwrap();
        with_home(home.path(), || {
            let (_tmp, repo, conflict_id) = seed_conflict_repo();
            record_resolution(
                &repo,
                &conflict_id,
                overwatch::merge_conflict::ResolveChoice::Theirs,
            );
            let cfg = test_cfg(&repo);
            let out = resolve_merge(&cfg, &repo, &conflict_id)
                .expect("theirs resolution should reconcile");
            assert_eq!(
                out,
                ResolveOutcome::Reconciled(overwatch::merge_conflict::ResolveChoice::Theirs)
            );
            assert!(!repo.join(".git").join("MERGE_HEAD").exists());
            let content = fs::read_to_string(repo.join("shared.txt")).unwrap();
            assert!(
                content.contains("branch version"),
                "theirs must take the feature branch content, got: {content}"
            );
        });
    }

    #[test]
    fn resolve_merge_manual_materializes_markers_for_a_human() {
        let home = TempDir::new().unwrap();
        with_home(home.path(), || {
            let (_tmp, repo, conflict_id) = seed_conflict_repo();
            record_resolution(
                &repo,
                &conflict_id,
                overwatch::merge_conflict::ResolveChoice::Manual,
            );
            let cfg = test_cfg(&repo);
            let out = resolve_merge(&cfg, &repo, &conflict_id)
                .expect("manual resolution should materialize markers, not error");
            match out {
                ResolveOutcome::ManualPending(files) => {
                    assert!(
                        files.iter().any(|f| f == "shared.txt"),
                        "manual pending must list the conflicted file, got {files:?}"
                    );
                }
                other => panic!("manual must leave markers pending, got {other:?}"),
            }
            // The merge is IN PROGRESS (markers materialized) for a human to finish.
            assert!(
                repo.join(".git").join("MERGE_HEAD").exists(),
                "manual must leave the in-progress merge with conflict markers"
            );
        });
    }

    /// Enqueue an OPEN `RuntimeOverlap` merge-hold for `branch` placed by
    /// `run_id`, exactly as the runtime-conflict hook does on a positive
    /// detection (no resolution → it stays open). Returns the `conflict_id`.
    fn seed_runtime_overlap_hold(repo: &Path, run_id: &str, branch: &str) -> String {
        let conflict_id = format!("{run_id}/{branch}/runtime-overlap");
        let entry = overwatch::merge_conflict::MergeConflictEntry {
            conflict_id: conflict_id.clone(),
            origin: overwatch::merge_conflict::ConflictOrigin::RuntimeOverlap,
            run_id: run_id.to_string(),
            branch: branch.to_string(),
            default_branch: "main".to_string(),
            base_ref: merge_base_sha(repo, "main", branch),
            conflicted_files: vec!["feat.txt".to_string()],
            diff_ours: "peer".to_string(),
            diff_theirs: "mine".to_string(),
            ts: overwatch::store::now(),
        };
        overwatch::store::append_merge_conflict(repo, &entry).unwrap();
        conflict_id
    }

    /// Decision A gate (finding #2a) + holder-run LIVENESS (e117d351): an OPEN
    /// `RuntimeOverlap` merge-hold whose HOLDER RUN IS STILL LIVE must HOLD the
    /// merge — `merge()` returns `MergeOutcome::Held`, NOT `Merged`.
    ///
    /// Liveness is seeded the SAME way production records it: the holder run
    /// `run` claims the task's file in condukt's OWN claim registry
    /// (`claim::claim_files`, exactly what `state set --status running` calls)
    /// with a FRESH heartbeat, so `claim::run_is_live` reads it as alive. There
    /// is NO fabricated cross-store alignment — no overwatch lease is seeded at
    /// all — so this ALSO pins that liveness is read from condukt's claim
    /// registry, not overwatch leases: against the reverted disjoint-lease
    /// implementation (which matched the hold's run_id against overwatch LEASE
    /// run_ids that condukt never populates) this hold would find no matching
    /// lease, be treated as dead, and the merge would come back `Merged` — this
    /// test would FAIL. It also FAILS if the pre-merge hold gate is deleted.
    #[test]
    fn merge_is_held_when_runtime_overlap_holder_run_is_live() {
        let (tmp, repo) = init_repo();
        make_branch(&repo, "feat", "feat.txt", "feature content\n");
        let cfg = test_cfg(&repo);
        let home = tmp.path().join("home-held-live");
        fs::create_dir_all(&home).unwrap();

        let outcome = with_home(&home, || {
            seed_runtime_overlap_hold(&repo, "run", "feat");
            // The holder run "run" is LIVE: it holds a file claim in condukt's
            // own registry with a FRESH heartbeat, precisely how a running task
            // records itself (`state set --status running` -> claim_files).
            crate::claim::claim_files(
                &cfg,
                &repo,
                "run",
                Some("sess"),
                &["feat.txt".to_string()],
                crate::state::now_secs(),
            )
            .unwrap();
            merge(&cfg, &repo, "feat", "main")
                .expect("a held overlap is a hold-for-review, not a hard error")
        });

        match outcome {
            MergeOutcome::Held(id) => assert_eq!(id, "run/feat/runtime-overlap"),
            other => panic!("a LIVE-holder runtime overlap must HOLD the merge, got {other:?}"),
        }
        // The held merge must NOT have landed the branch.
        assert!(
            !repo.join("feat.txt").exists(),
            "a HELD merge must not land the branch onto the default branch"
        );
    }

    /// DEAD holder by ABSENCE (e117d351): an OPEN `RuntimeOverlap` hold whose
    /// holder run holds NO claim in condukt's registry (a fully
    /// abandoned/crashed run whose claims were released or never re-recorded)
    /// must NOT block a task that REUSES the same `condukt/<task.id>` branch
    /// name. The stale hold is filtered by holder-run liveness, so `merge()`
    /// proceeds and returns `Merged`. RED without the liveness filter (the hold
    /// matches by branch name and the merge comes back `Held`); GREEN with it.
    #[test]
    fn merge_not_held_when_runtime_overlap_holder_run_has_no_claim() {
        let (tmp, repo) = init_repo();
        make_branch(&repo, "feat", "feat.txt", "feature content\n");
        let cfg = test_cfg(&repo);
        let home = tmp.path().join("home-dead-absent");
        fs::create_dir_all(&home).unwrap();

        let outcome = with_home(&home, || {
            // Hold placed by "deadrun"; that run holds NO claim → not live.
            seed_runtime_overlap_hold(&repo, "deadrun", "feat");
            merge(&cfg, &repo, "feat", "main")
                .expect("a filtered stale hold must let the merge proceed, not error")
        });

        assert_eq!(
            outcome,
            MergeOutcome::Merged,
            "a hold from a holder run with no live claim must NOT block the reused-branch merge"
        );
        assert!(
            repo.join("feat.txt").exists(),
            "the reused-branch merge must land once the stale hold is filtered"
        );
    }

    /// DEAD holder by TTL EXPIRY (e117d351): the same filtering, but proving the
    /// heartbeat-TTL staleness path. The holder run DID record a claim, but its
    /// heartbeat is aged past the stuck-TTL (the run crashed without releasing),
    /// so `claim::run_is_live` reads it as dead via `is_stale`. TTL expiry is
    /// simulated deterministically by back-dating the claim's heartbeat rather
    /// than sleeping. The hold must not block → `Merged`.
    #[test]
    fn merge_not_held_when_runtime_overlap_holder_claim_is_ttl_expired() {
        let (tmp, repo) = init_repo();
        make_branch(&repo, "feat", "feat.txt", "feature content\n");
        let cfg = test_cfg(&repo);
        let home = tmp.path().join("home-dead-ttl");
        fs::create_dir_all(&home).unwrap();

        let outcome = with_home(&home, || {
            seed_runtime_overlap_hold(&repo, "deadrun", "feat");
            // "deadrun" DID claim, but its last heartbeat is well past the TTL
            // (recorded at now - stuck_ttl - 100): a genuinely dead run.
            let stale_at = crate::state::now_secs() - cfg.stuck_ttl_secs as i64 - 100;
            crate::claim::claim_files(
                &cfg,
                &repo,
                "deadrun",
                Some("sess"),
                &["feat.txt".to_string()],
                stale_at,
            )
            .unwrap();
            merge(&cfg, &repo, "feat", "main")
                .expect("a filtered TTL-expired hold must let the merge proceed, not error")
        });

        assert_eq!(
            outcome,
            MergeOutcome::Merged,
            "a hold whose holder-run claim is TTL-expired must NOT block the reused-branch merge"
        );
        assert!(
            repo.join("feat.txt").exists(),
            "the reused-branch merge must land once the TTL-expired hold is filtered"
        );
    }

    /// Anti-regression pin (e117d351): liveness is read from condukt's OWN claim
    /// registry, NOT from overwatch leases. This is the exact trap the reverted
    /// disjoint-lease implementation fell into. Here we FABRICATE the very
    /// alignment that implementation relied on — a FRESH overwatch lease carrying
    /// the hold's run_id — but record NO condukt claim. The reverted
    /// `hold_placed_by_live_run` (load_leases + is_stale) would read this lease
    /// as live and return `Held`; the correct condukt-registry check finds no
    /// live claim and returns `Merged`. Asserting `Merged` proves the gate
    /// ignores overwatch leases entirely and reads condukt's own registry.
    #[test]
    fn runtime_overlap_liveness_ignores_overwatch_leases() {
        let (tmp, repo) = init_repo();
        make_branch(&repo, "feat", "feat.txt", "feature content\n");
        let cfg = test_cfg(&repo);
        let home = tmp.path().join("home-lease-ignored");
        fs::create_dir_all(&home).unwrap();

        let outcome = with_home(&home, || {
            seed_runtime_overlap_hold(&repo, "leaserun", "feat");
            // Fabricate a FRESH overwatch lease for the holder run (the reverted
            // impl's liveness source) — but record NO condukt claim.
            let mut leases = overwatch::store::load_leases(&repo).unwrap_or_default();
            leases.insert(
                "leaserun/task".to_string(),
                overwatch::store::Lease {
                    key: "leaserun/task".to_string(),
                    title: "t".to_string(),
                    session_id: "sess".to_string(),
                    run_id: "leaserun".to_string(),
                    claimed_at: overwatch::store::now(),
                    heartbeat_at: overwatch::store::now(),
                    scope: Vec::new(),
                    done_criteria: None,
                },
            );
            overwatch::store::save_leases(&repo, &leases).unwrap();
            merge(&cfg, &repo, "feat", "main")
                .expect("a hold with no condukt-registry liveness must merge, not error")
        });

        assert_eq!(
            outcome,
            MergeOutcome::Merged,
            "a fresh overwatch lease must NOT keep a hold alive; liveness reads condukt's claim registry"
        );
        assert!(
            repo.join("feat.txt").exists(),
            "the merge must land: no live condukt claim carries the hold's run_id"
        );
    }

    /// FAIL-CLOSED on undetermined liveness (e117d351 hardening): if the claim
    /// registry exists but is UNREADABLE/corrupt at merge time, the hold's
    /// placer-run liveness cannot be established — so the hold must KEEP blocking
    /// (`Held`), NOT be dropped. Dropping it here would re-enter the reverted
    /// disjoint-id bug's failure class (an unreadable/empty registry making every
    /// hold look dead ⇒ the gate silently off), just via registry corruption
    /// instead of id mismatch. RED against the fail-soft `load()`-to-empty
    /// behavior (corrupt ⇒ "not live" ⇒ `Merged`); GREEN once liveness is
    /// three-valued and the gate fails closed on `Undetermined`.
    #[test]
    fn merge_stays_held_when_claim_registry_is_unreadable() {
        let (tmp, repo) = init_repo();
        make_branch(&repo, "feat", "feat.txt", "feature content\n");
        let cfg = test_cfg(&repo);
        let home = tmp.path().join("home-corrupt-registry");
        fs::create_dir_all(&home).unwrap();

        let outcome = with_home(&home, || {
            seed_runtime_overlap_hold(&repo, "run", "feat");
            // Corrupt the very registry `run_liveness` reads: liveness for "run"
            // is now UNDETERMINED (not positively dead), so the gate must hold.
            let reg = crate::claim::registry_path_for_test(&cfg, &repo);
            fs::create_dir_all(reg.parent().unwrap()).unwrap();
            fs::write(&reg, b"{ not valid json ]]").unwrap();
            merge(&cfg, &repo, "feat", "main")
                .expect("an undetermined-liveness hold is a hold-for-review, not a hard error")
        });

        match outcome {
            MergeOutcome::Held(id) => assert_eq!(id, "run/feat/runtime-overlap"),
            other => panic!("an unreadable registry must FAIL CLOSED (hold), got {other:?}"),
        }
        assert!(
            !repo.join("feat.txt").exists(),
            "the merge must NOT land while holder-run liveness is undetermined"
        );
    }

    /// Cleanup wiring (finding #1 + #2b): once a peer task's branch LANDS, its
    /// changeset must leave the overlap-detection set so the next sequential task
    /// that touches a common file is NOT spuriously Held. This test FAILS if the
    /// on-merge cleanup (`finalize_landed_branch` → `mark_branch_merged`) is
    /// absent: `record_changeset_and_detect` for t2 would then still see t1 as
    /// in-flight and emit an overlap event, the mimic'd hook would enqueue a
    /// hold, and t2's merge would come back `Held` instead of `Merged`.
    #[test]
    fn a_landed_peer_does_not_spuriously_hold_a_later_overlapping_merge() {
        use overwatch::changeset::ActualChangeset;
        let (tmp, repo) = init_repo();
        // t1 adds `common.rs`; merges cleanly into main.
        make_branch(&repo, "feat1", "common.rs", "t1 content\n");
        let cfg = test_cfg(&repo);
        let home = tmp.path().join("home-land");
        fs::create_dir_all(&home).unwrap();

        with_home(&home, || {
            // t1 records its ACTUAL changeset (touches common.rs), UNMERGED.
            let base1 = merge_base_sha(&repo, "main", "feat1");
            let cs1 = ActualChangeset::new(
                "run/t1".to_string(),
                "run".to_string(),
                "sess".to_string(),
                "feat1".to_string(),
                base1,
                "head1".to_string(),
                &["common.rs".to_string()],
                overwatch::store::now(),
            );
            let ev1 = overwatch::store::record_changeset_and_detect(&repo, &cs1).unwrap();
            assert!(ev1.is_empty(), "t1 is first in flight; no overlap yet");

            // t1 lands cleanly. The #1 cleanup wiring must mark t1's changeset
            // merged so it stops counting as in-flight.
            assert_eq!(
                merge(&cfg, &repo, "feat1", "main").unwrap(),
                MergeOutcome::Merged,
                "t1 must merge cleanly"
            );

            // t2 (a later, separate branch) records an OVERLAPPING changeset
            // (also common.rs). Because t1 already LANDED, detection must find
            // NO overlap → no hold is enqueued.
            let cs2 = ActualChangeset::new(
                "run/t2".to_string(),
                "run".to_string(),
                "sess".to_string(),
                "feat2".to_string(),
                "base2".to_string(),
                "head2".to_string(),
                &["common.rs".to_string()],
                overwatch::store::now(),
            );
            let ev2 = overwatch::store::record_changeset_and_detect(&repo, &cs2).unwrap();
            assert!(
                ev2.is_empty(),
                "a LANDED peer must be excluded from overlap detection; got {ev2:?}"
            );
            // Mimic the runtime-conflict hook: a hold is enqueued ONLY on a
            // positive detection. Without the fix ev2 is non-empty → this fires.
            if !ev2.is_empty() {
                let hold = overwatch::merge_conflict::MergeConflictEntry {
                    conflict_id: "run/t2/runtime-overlap".to_string(),
                    origin: overwatch::merge_conflict::ConflictOrigin::RuntimeOverlap,
                    run_id: "run".to_string(),
                    branch: "feat2".to_string(),
                    default_branch: "main".to_string(),
                    base_ref: "base2".to_string(),
                    conflicted_files: vec!["common.rs".to_string()],
                    diff_ours: "peer".to_string(),
                    diff_theirs: "mine".to_string(),
                    ts: overwatch::store::now(),
                };
                overwatch::store::append_merge_conflict(&repo, &hold).unwrap();
            }

            // A real clean-merging feat2 branch; with no hold it must merge.
            make_branch(&repo, "feat2", "feat2.txt", "t2 content\n");
            assert_eq!(
                merge(&cfg, &repo, "feat2", "main").unwrap(),
                MergeOutcome::Merged,
                "a landed peer must NOT spuriously hold t2's merge"
            );
        });
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

    // ── Repo-primary lock: cannot-acquire must resolve to the RESTRICTIVE side ──
    //
    // Fault injection (shared by all four tests below): point the lock's
    // `state_dir` at a path that is a regular FILE, so `acquire_at`'s
    // `create_dir_all(<state_dir>/<project-key>)` fails with `NotADirectory`.
    // That is a genuine, deterministic acquisition failure — the same
    // "cannot determine whether a peer holds this lock" condition an I/O error
    // or a 10s contention timeout produces — without a timing-dependent wait.

    /// A `state_dir` that can never host a lock file (its own path is a FILE).
    /// Returns the wedged dir; the caller keeps `TempDir` alive.
    fn wedged_state_dir(tmp: &Path) -> PathBuf {
        let p = tmp.join("wedged-state");
        fs::write(&p, b"not a directory\n").unwrap();
        p
    }

    /// A `$HOME` whose `~/.condukt/state` is a FILE, wedging the lock for the
    /// `*_loaded` callers (`create` / `discard`) that build their own `Config`.
    fn wedged_home(tmp: &Path) -> PathBuf {
        let home = tmp.join("wedged-home");
        fs::create_dir_all(home.join(".condukt")).unwrap();
        fs::write(home.join(".condukt").join("state"), b"not a directory\n").unwrap();
        home
    }

    /// `merge` checks out the default branch and commits a merge onto it in the
    /// PRIMARY working tree. Running that unlocked can interleave with a peer
    /// `repo commit`'s staging or another run's merge/prune. Cannot-acquire must
    /// therefore REFUSE — and refuse BEFORE any git mutation.
    #[test]
    fn merge_refuses_when_repo_primary_lock_cannot_be_acquired() {
        let (tmp, repo) = init_repo();
        make_branch(&repo, "feat", "feat.txt", "feature content\n");

        let side = TempDir::new().unwrap();
        let mut cfg = test_cfg(&repo);
        cfg.state_dir = wedged_state_dir(side.path());

        let home = tmp.path().join("home-merge-refuse");
        fs::create_dir_all(&home).unwrap();
        let err = with_home(&home, || merge(&cfg, &repo, "feat", "main"))
            .expect_err("merge must REFUSE when the repo-primary lock cannot be acquired");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("repo-primary lock"),
            "the refusal must name the repo-primary lock, got: {msg}"
        );
        assert!(
            !repo.join("feat.txt").exists(),
            "a refused merge must not land the branch's content on the default branch"
        );
    }

    /// `resolve_merge` performs the SAME default-branch mutation as `merge`
    /// (checkout + `merge -s ours` / `-X theirs` / markers). Same verdict:
    /// refuse. The assertion is on the MESSAGE, not merely `is_err()`, because
    /// this path already errors for an unrelated reason ("no merge-conflict
    /// entry") — proving the lock refusal happens FIRST, before the store read.
    #[test]
    fn resolve_merge_refuses_when_repo_primary_lock_cannot_be_acquired() {
        let (tmp, repo) = init_repo();

        let side = TempDir::new().unwrap();
        let mut cfg = test_cfg(&repo);
        cfg.state_dir = wedged_state_dir(side.path());

        let home = tmp.path().join("home-resolve-refuse");
        fs::create_dir_all(&home).unwrap();
        let err = with_home(&home, || resolve_merge(&cfg, &repo, "nope/merge-conflict"))
            .expect_err("resolve_merge must REFUSE when the repo-primary lock cannot be acquired");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("repo-primary lock"),
            "the lock refusal must precede the store read, got: {msg}"
        );
    }

    /// `create` holds the lock across `worktree prune` + `branch -D` (a REF
    /// deletion) + `worktree add`. Unlocked, its `branch -D` can drop a ref a
    /// concurrent merge is landing, and its prune/add can race another run's
    /// worktree admin mutations. Cannot-acquire must refuse — and create nothing.
    #[test]
    fn create_refuses_when_repo_primary_lock_cannot_be_acquired() {
        let (tmp, repo) = init_repo();
        let wt = TempDir::new().unwrap();
        let home = wedged_home(tmp.path());

        let err = with_home(&home, || create(&repo, wt.path(), "t1", "condukt/t1"))
            .expect_err("create must REFUSE when the repo-primary lock cannot be acquired");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("repo-primary lock"),
            "the refusal must name the repo-primary lock, got: {msg}"
        );
        assert!(
            !wt.path().join("t1").exists(),
            "a refused create must not register a worktree"
        );
    }

    /// `discard` force-removes a worktree, prunes, and `branch -D`s — the most
    /// destructive primary-repo mutation condukt performs. Unlocked it can
    /// delete a branch ref a concurrent merge is reading. Cannot-acquire must
    /// refuse; leaving the worktree on disk is recoverable, deleting a branch
    /// during a peer's merge is not.
    #[test]
    fn discard_refuses_when_repo_primary_lock_cannot_be_acquired() {
        let (tmp, repo) = init_repo();
        let wt = TempDir::new().unwrap();
        let path = wt.path().join("exp");
        git(
            &repo,
            &["worktree", "add", "-b", "exp", "--", path.to_str().unwrap()],
        )
        .unwrap();
        let home = wedged_home(tmp.path());

        let err = with_home(&home, || discard(&repo, &path, Some("exp")))
            .expect_err("discard must REFUSE when the repo-primary lock cannot be acquired");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("repo-primary lock"),
            "the refusal must name the repo-primary lock, got: {msg}"
        );
        assert!(
            path.exists(),
            "a refused discard must leave the worktree in place"
        );
        let (branch_alive, _, _) = git_try(
            &repo,
            &["rev-parse", "--verify", "--quiet", "refs/heads/exp"],
        )
        .unwrap();
        assert!(
            branch_alive,
            "a refused discard must NOT force-delete the branch ref unlocked"
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
    // on the shared repo-scoped lock. Held for the whole function.
    // (`_loaded`: discard() doesn't thread a Config; callers are in sibling
    // modules out of this change's scope.)
    //
    // Cannot-acquire REFUSES (`?`). This is condukt's most destructive
    // primary-repo mutation — an unrecoverable `branch -D` — so proceeding
    // unlocked could drop commits a peer's merge is mid-way through reading.
    // The cost of refusing is bounded and reversible: the experiment worktree
    // stays on disk (visible to `worktree cleanup`) and the discard is retried.
    let _repo_lock = lock::acquire_repo_primary_loaded(repo)?;
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
    fn namespaced_topic_stays_a_single_validated_component() {
        // The spliced topic must still satisfy validate_topic — otherwise
        // create() would bail on our own construction. In particular it must NOT
        // gain a path separator (orphans() only scans one level of
        // worktree_base, and validate_topic is a traversal guard).
        let t = namespaced_topic("run-20260723-101112", "t1");
        assert_eq!(t, "run-20260723-101112-t1");
        assert!(
            validate_topic(&t).is_ok(),
            "spliced topic must validate: {t}"
        );
        assert!(
            !t.contains('/'),
            "spliced topic must stay one component: {t}"
        );

        // Two runs, SAME task id -> different dirs. This is the whole point.
        assert_ne!(
            namespaced_topic("runA", "t1"),
            namespaced_topic("runB", "t1")
        );
    }

    #[test]
    fn namespaced_branch_inserts_the_run_segment_and_never_matches_a_peer() {
        assert_eq!(namespaced_branch("runA", "condukt/t1"), "condukt/runA/t1");
        // No '/' in the input branch: the run becomes the leading segment.
        assert_eq!(namespaced_branch("runA", "t1"), "runA/t1");
        // Deeper prefixes keep their shape.
        assert_eq!(namespaced_branch("runA", "a/b/t1"), "a/b/runA/t1");

        for b in [
            namespaced_branch("runA", "condukt/t1"),
            namespaced_branch("runA", "t1"),
            namespaced_branch("runA", "a/b/t1"),
        ] {
            assert!(
                validate_branch(&b).is_ok(),
                "spliced branch must validate: {b}"
            );
        }

        // Two runs emitting the same task id must NEVER produce the same ref —
        // that equality is exactly what let create()'s stale-ref `branch -D`
        // destroy a peer session's unmerged commits.
        assert_ne!(
            namespaced_branch("runA", "condukt/t1"),
            namespaced_branch("runB", "condukt/t1")
        );
        // And neither may equal the LEGACY un-namespaced ref, so old branches
        // are never re-targeted by the new naming.
        assert_ne!(namespaced_branch("runA", "condukt/t1"), "condukt/t1");
    }

    #[test]
    fn validate_run_ns_rejects_separators_and_option_injection() {
        assert!(validate_run_ns("run-20260723-101112").is_ok());
        assert!(validate_run_ns("").is_err());
        assert!(
            validate_run_ns("a/b").is_err(),
            "a '/' in the run id would escape the single-component topic"
        );
        assert!(validate_run_ns("..").is_err());
        assert!(validate_run_ns("../evil").is_err());
        assert!(validate_run_ns("-rf").is_err());
        assert!(validate_run_ns(".hidden").is_err());
        assert!(validate_run_ns("a b").is_err());
        assert!(validate_run_ns("a;rm -rf").is_err());
    }

    #[test]
    fn create_namespaced_without_a_run_is_byte_identical_to_legacy_create() {
        // Backward compatibility: an older driver (or any caller that does not
        // pass --run) must land on exactly the legacy path/branch.
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        let base = tmp.path().join("wt");
        std::fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);

        let p = create_namespaced(&repo, &base, None, "t1", "condukt/t1").expect("legacy create");
        assert_eq!(
            p,
            base.join("t1"),
            "legacy layout must remain <worktree_base>/<topic>"
        );
        assert!(
            git(&repo, &["rev-parse", "--verify", "refs/heads/condukt/t1"]).is_ok(),
            "legacy branch name must be used verbatim"
        );
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
