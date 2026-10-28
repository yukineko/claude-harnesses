//! Which files did the working tree touch? Any git failure is `Err`, never an
//! empty list (an empty list reads as "nothing touched").

use std::path::Path;
use std::process::Command;

fn git_z(root: &Path, args: &[&str]) -> Result<Vec<String>, String> {
    // No pager, and quoting pinned to git's default (irrelevant under `-z`,
    // pinned anyway). Diff calls additionally pass `--no-ext-diff
    // --no-textconv --no-color` so no config-defined driver shapes the output.
    let out = Command::new("git")
        .args(["-c", "core.quotePath=true", "--no-pager"])
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`git {}` exited with {:?}: {}",
            args.join(" "),
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out
        .stdout
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect())
}

/// Paths (relative to `root`) changed vs HEAD (staged or not, deleted and
/// renamed-from included) plus untracked, non-ignored files.
pub fn touched_files(root: &Path) -> Result<Vec<String>, String> {
    let mut v = git_z(
        root,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--name-only",
            "-z",
            "--no-renames",
            "--relative",
            "HEAD",
        ],
    )?;
    v.extend(git_z(
        root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?);
    v.sort();
    v.dedup();
    Ok(v)
}

/// Why the git top level could not be resolved.
pub enum TopLevelErr {
    /// Observed: git ran and reported `dir` is not inside a repository, so no
    /// top level exists and `dir` itself is the only candidate root.
    NotARepo(String),
    /// Anything else (git missing, crashed, unexpected output): a top level
    /// may exist but is unknown.
    Failed(String),
}

impl TopLevelErr {
    pub fn message(&self) -> &str {
        match self {
            TopLevelErr::NotARepo(m) | TopLevelErr::Failed(m) => m,
        }
    }
}

/// `git rev-parse --show-toplevel` of `dir`. Runs with `LC_ALL=C` so the
/// "not a git repository" message is recognisable; an unrecognised failure is
/// [`TopLevelErr::Failed`].
pub fn toplevel(dir: &Path) -> Result<std::path::PathBuf, TopLevelErr> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir)
        .env("LC_ALL", "C")
        .output()
        .map_err(|e| TopLevelErr::Failed(format!("cannot run git: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let msg = format!(
            "`git rev-parse --show-toplevel` in {} exited with {:?}: {}",
            dir.display(),
            out.status.code(),
            stderr.trim()
        );
        return Err(
            if out.status.code() == Some(128) && stderr.contains("not a git repository") {
                TopLevelErr::NotARepo(msg)
            } else {
                TopLevelErr::Failed(msg)
            },
        );
    }
    let s = String::from_utf8(out.stdout)
        .map_err(|e| TopLevelErr::Failed(format!("git top level is not UTF-8: {e}")))?;
    let s = s.trim_end_matches(['\n', '\r']);
    if s.is_empty() {
        return Err(TopLevelErr::Failed(
            "`git rev-parse --show-toplevel` printed nothing".to_string(),
        ));
    }
    Ok(std::path::PathBuf::from(s))
}
