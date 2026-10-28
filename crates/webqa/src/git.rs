//! Which files did the working tree touch? Any git failure is `Err`, never an
//! empty list (an empty list reads as "nothing touched").

use std::path::Path;
use std::process::Command;

fn git_z(root: &Path, args: &[&str]) -> Result<Vec<String>, String> {
    let out = Command::new("git")
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
