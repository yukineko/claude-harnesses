//! The git probes fde makes: "which git work tree top-level contains this
//! directory?" ([`show_toplevel`]) and "which work trees does the repository
//! of this directory have?" ([`worktree_paths`], used by the PreToolUse gate).
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
//! path are all `Undetermined`. There is no fallback to `dir` itself.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use harness_core::boundary;
use harness_core::verdict::Determination;

/// Upper bound on the probe. `rev-parse --show-toplevel` is local and
/// read-only; a git that has not answered by then is undetermined.
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
