//! The git probes fde makes: "which git work tree top-level contains this
//! directory?" ([`show_toplevel`]), "which work trees does the repository
//! of this directory have?" ([`worktree_paths`], used by the PreToolUse gate),
//! and the target HEAD provenance probes recorded with a ratification and
//! with every evidence record: "which commit is HEAD?" ([`head`]), "does the
//! work tree differ from it?" ([`is_dirty`]) and "is commit A an ancestor of,
//! or equal to, commit B?" ([`is_ancestor`]).
//!
//! Each probe runs git with `current_dir(dir)` and
//! every inherited repo-relocating `GIT_*` variable removed
//! (`harness_core::git_env::scrub`: `GIT_DIR`, `GIT_WORK_TREE`,
//! `GIT_INDEX_FILE`, `GIT_COMMON_DIR`, `GIT_CEILING_DIRECTORIES`, …), so the
//! repository is discovered from `dir` alone and never from a `GIT_DIR` that a
//! git hook or the user's shell exported.
//!
//! The answer is decided by the subprocess **exit status** first: a spawn
//! failure, a timeout, a non-zero exit (e.g. "not a git repository"), an
//! unreadable stdout, or an exit-0 answer that is empty or not an absolute
//! path are all `Undetermined`. There is no fallback to `dir` itself. The
//! provenance probes follow the same rule (their exact answers are documented
//! on each function); [`is_ancestor`] is the only probe for which a non-zero
//! exit is an answer (exit 1 = "not an ancestor"), and every other non-zero
//! exit of it is still `Undetermined`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use harness_core::boundary;
use harness_core::verdict::Determination;

/// Upper bound on each probe. Every probe here is local and read-only; a git
/// that has not answered by then is undetermined.
const GIT_TIMEOUT: Duration = Duration::from_secs(10);

/// The git work tree top-level containing `dir`, exactly as git reports it
/// (not yet canonicalized; the caller canonicalizes).
///
/// `Known(path)` only when git exited 0 and printed a non-empty absolute path.
/// Everything else is `Undetermined` with the reason.
pub fn show_toplevel(dir: &Path) -> Determination<PathBuf> {
    let mut cmd = Command::new("git");
    harness_core::git_env::scrub(&mut cmd);
    cmd.current_dir(dir)
        .args(["rev-parse", "--show-toplevel"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let out = match boundary::run_with_timeout(&mut cmd, GIT_TIMEOUT) {
        Determination::Known(out) => out,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    // `stdout_on_success` answers `Undetermined` for every non-zero exit: a
    // directory outside any work tree is a refusal, not an empty answer.
    let text = match out.stdout_on_success() {
        Determination::Known(t) => t,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    // git terminates the path with exactly one newline; strip only that, so a
    // path is never shortened beyond what git printed.
    let line = match text.strip_suffix('\n') {
        Some(l) => l,
        None => text.as_str(),
    };
    if line.is_empty() {
        return Determination::undetermined(format!(
            "`git rev-parse --show-toplevel` in {} exited 0 but printed no top-level",
            dir.display()
        ));
    }
    let top = PathBuf::from(line);
    if !top.is_absolute() {
        return Determination::undetermined(format!(
            "`git rev-parse --show-toplevel` in {} printed a non-absolute path: {line:?}",
            dir.display()
        ));
    }
    Determination::known(top)
}

/// Every work tree of the repository whose work tree contains `dir` (the main
/// work tree and every linked one), exactly as
/// `git worktree list --porcelain -z` reports them (not canonicalized).
///
/// Runs with the same scrubbed environment and timeout as [`show_toplevel`].
/// `Known(paths)` only when git exited 0 and every record names an absolute
/// path; a spawn failure, timeout, non-zero exit, a record without a path, a
/// non-absolute path, or an empty list (git always lists the main work tree)
/// is `Undetermined`.
pub fn worktree_paths(dir: &Path) -> Determination<Vec<PathBuf>> {
    let mut cmd = Command::new("git");
    harness_core::git_env::scrub(&mut cmd);
    cmd.current_dir(dir)
        .args(["worktree", "list", "--porcelain", "-z"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let out = match boundary::run_with_timeout(&mut cmd, GIT_TIMEOUT) {
        Determination::Known(out) => out,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let text = match out.stdout_on_success() {
        Determination::Known(t) => t,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let mut paths = Vec::new();
    for field in text.split('\0') {
        if let Some(p) = field.strip_prefix("worktree ") {
            let p = PathBuf::from(p);
            if !p.is_absolute() {
                return Determination::undetermined(format!(
                    "`git worktree list` in {} printed a non-absolute work tree {}",
                    dir.display(),
                    p.display()
                ));
            }
            paths.push(p);
        } else if field == "worktree" {
            return Determination::undetermined(format!(
                "`git worktree list` in {} printed a work tree record without a path",
                dir.display()
            ));
        }
    }
    if paths.is_empty() {
        return Determination::undetermined(format!(
            "`git worktree list` in {} exited 0 but listed no work tree",
            dir.display()
        ));
    }
    Determination::known(paths)
}

/// A git command run at `dir` with the repo-relocating `GIT_*` variables
/// removed, stdin null and both output pipes captured.
fn git_at(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    harness_core::git_env::scrub(&mut cmd);
    cmd.current_dir(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// A full commit id as fde records it: exactly 40 lowercase hex digits (a
/// SHA-1 object name). Anything else — including a SHA-256 repository's
/// 64-digit ids — is not one.
pub fn is_commit_id(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The commit HEAD of the repository at `dir` resolves to, as printed by
/// `git rev-parse --verify HEAD`.
///
/// `Known(id)` only when git exited 0 and printed exactly one 40-digit
/// lowercase hex id (plus one trailing newline). A spawn failure, a timeout, a
/// non-zero exit (e.g. an unborn HEAD in a repository without commits, which
/// `--verify` refuses), or any other output is `Undetermined`.
pub fn head(dir: &Path) -> Determination<String> {
    let mut cmd = git_at(dir, &["rev-parse", "--verify", "HEAD"]);
    let out = match boundary::run_with_timeout(&mut cmd, GIT_TIMEOUT) {
        Determination::Known(out) => out,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let text = match out.stdout_on_success() {
        Determination::Known(t) => t,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let line = match text.strip_suffix('\n') {
        Some(l) => l,
        None => text.as_str(),
    };
    if !is_commit_id(line) {
        return Determination::undetermined(format!(
            "`git rev-parse --verify HEAD` in {} printed {line:?}, not a 40-digit lowercase \
             hex commit id",
            dir.display()
        ));
    }
    Determination::known(line.to_owned())
}

/// Whether the work tree at `dir` differs from HEAD: `true` when
/// `git --no-optional-locks status --porcelain --untracked-files=normal
/// --ignored=no` printed anything.
///
/// Modified, staged, deleted and untracked (not ignored) files make the tree
/// dirty; files ignored by `.gitignore` / `info/exclude` /
/// `core.excludesFile` do not. The untracked and ignored modes are passed
/// explicitly so a `status.showUntrackedFiles` setting cannot hide untracked
/// files. `--no-optional-locks` keeps the observation from refreshing
/// (writing) the index, so probing does not modify the repository. `Known`
/// only when git exited 0; a spawn failure, timeout or non-zero exit is
/// `Undetermined`.
pub fn is_dirty(dir: &Path) -> Determination<bool> {
    let mut cmd = git_at(
        dir,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain",
            "--untracked-files=normal",
            "--ignored=no",
        ],
    );
    let out = match boundary::run_with_timeout(&mut cmd, GIT_TIMEOUT) {
        Determination::Known(out) => out,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    match out.stdout_on_success() {
        Determination::Known(t) => Determination::known(!t.is_empty()),
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

/// Whether commit `ancestor` is an ancestor of, or equal to, commit
/// `descendant` in the repository at `dir`, by
/// `git merge-base --is-ancestor <ancestor> <descendant>`.
///
/// * `Known(true)` — git exited 0.
/// * `Known(false)` — git exited 1 (determined: not an ancestor, e.g. after a
///   reset, a branch switch, rewritten or unrelated history).
/// * `Undetermined` — either argument is not a 40-digit lowercase hex id
///   (checked before git runs, so neither can be read as an option), or git
///   could not be run, timed out, or exited with any other code (e.g. 128 for
///   a commit unknown to the repository).
pub fn is_ancestor(ancestor: &str, descendant: &str, dir: &Path) -> Determination<bool> {
    for id in [ancestor, descendant] {
        if !is_commit_id(id) {
            return Determination::undetermined(format!(
                "cannot compare commits in {}: {id:?} is not a 40-digit lowercase hex commit id",
                dir.display()
            ));
        }
    }
    let mut cmd = git_at(dir, &["merge-base", "--is-ancestor", ancestor, descendant]);
    let out = match boundary::run_with_timeout(&mut cmd, GIT_TIMEOUT) {
        Determination::Known(out) => out,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    match out.code() {
        0 => Determination::known(true),
        1 => Determination::known(false),
        code => {
            let tail: String = out.stderr().chars().take(400).collect();
            Determination::undetermined(format!(
                "`git merge-base --is-ancestor {ancestor} {descendant}` in {} exited {code}; \
                 the commits could not be compared. stderr: {}",
                dir.display(),
                if tail.is_empty() { "<empty>" } else { &tail }
            ))
        }
    }
}
