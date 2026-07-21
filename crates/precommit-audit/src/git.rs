//! Thin wrappers over the `git` CLI. We shell out rather than link libgit2 so
//! the binary stays small and matches the exact semantics of the original hook
//! (which also shelled out). All output is decoded as UTF-8 (lossy) — git emits
//! UTF-8 on every platform, which sidesteps the CP932 mojibake the PowerShell
//! version had to guard against.

use std::path::Path;
use std::process::Command;

/// Outcome of a git invocation: exit code (None if killed by signal) plus decoded
/// stdout/stderr. Callers decide which non-zero exits are legitimate signals
/// (`grep -l` exit 1 = no match, `diff --no-index` exit 1 = files differ) vs errors.
struct GitOut {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Run `git <args>` in `cwd`. Returns `Err` ONLY on spawn failure (git not
/// runnable) — a hard, undetermined condition no caller may treat as "no output".
/// On any exit it returns `Ok(GitOut)` with the code so the caller can classify it.
fn run_git(cwd: &Path, args: &[&str]) -> Result<GitOut, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("failed to run `git {}`: {e}", args.join(" ")))?;
    Ok(GitOut {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Format a non-zero / signal exit into a reason string carrying git's stderr.
fn exit_reason(args: &[&str], o: &GitOut) -> String {
    let code = o
        .code
        .map(|c| c.to_string())
        .unwrap_or_else(|| "signal".to_string());
    let mut r = format!("`git {}` exited with {code}", args.join(" "));
    let err = o.stderr.trim();
    if !err.is_empty() {
        r.push_str(": ");
        r.push_str(err);
    }
    r
}

/// Run git and REQUIRE success (exit 0). A non-zero exit or spawn failure is a hard
/// error carrying git's stderr. Callers must NOT conflate this with "no output":
/// a failed `git diff` that silently became an empty working set — letting the audit
/// report a clean pass — is exactly the fail-open this closes.
fn git_ok(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let o = run_git(cwd, args)?;
    if o.code == Some(0) {
        Ok(o.stdout)
    } else {
        Err(exit_reason(args, &o))
    }
}

/// Files changed vs HEAD plus untracked files (excluding standard ignores),
/// de-duplicated and sorted. This is the audit's working set.
///
/// Fails closed: a git error here means the working set is UNKNOWN, not empty —
/// callers must never treat `Err` as "nothing changed".
pub fn changed_and_untracked(cwd: &Path) -> Result<Vec<String>, String> {
    let mut set: Vec<String> = Vec::new();
    for args in [
        &["diff", "--name-only", "HEAD"][..],
        &["ls-files", "--others", "--exclude-standard"][..],
    ] {
        let src = git_ok(cwd, args)?;
        for line in src.lines() {
            let l = line.trim_end_matches('\r');
            if !l.is_empty() {
                set.push(l.to_string());
            }
        }
    }
    set.sort();
    set.dedup();
    Ok(set)
}

/// Untracked files only (used by the diff-hash computation).
///
/// Fails closed: propagates a git error instead of silently reporting no
/// untracked files.
pub fn untracked(cwd: &Path) -> Result<Vec<String>, String> {
    Ok(
        git_ok(cwd, &["ls-files", "--others", "--exclude-standard"])?
            .lines()
            .map(|l| l.trim_end_matches('\r').to_string())
            .filter(|l| !l.is_empty())
            .collect(),
    )
}

/// Full `git diff HEAD` over the whole tree (used by the diff-hash computation).
///
/// Fails closed: propagates a git error instead of silently returning an empty
/// diff that would hash the same as a genuinely clean tree.
pub fn diff_head(cwd: &Path) -> Result<String, String> {
    git_ok(cwd, &["diff", "HEAD"])
}

/// Unified diff for a single file. Falls back to `--no-index` against /dev/null
/// for untracked files so their full content shows up as added lines.
///
/// Fails closed: propagates a git error instead of silently returning an empty
/// diff that would drop the file's added lines from every pattern scan.
pub fn file_diff(cwd: &Path, file: &str) -> Result<String, String> {
    let d = git_ok(cwd, &["diff", "HEAD", "--", file])?;
    if !d.trim().is_empty() {
        return Ok(d);
    }
    // Untracked: diff against the empty device. `--no-index` exits 1 when files
    // differ (the normal case here) and 0 when identical; only code >= 2 or a spawn
    // failure is a real error.
    let args = &["diff", "--no-index", "/dev/null", file];
    let o = run_git(cwd, args)?;
    match o.code {
        Some(0) | Some(1) => Ok(o.stdout),
        _ => Err(exit_reason(args, &o)),
    }
}

/// Added lines of a file's diff: lines starting with `+` (but not `+++`), with
/// any `audit-ignore: <reason>` line removed. The leading `+` is preserved so
/// pattern regexes anchored at `^\+` keep working.
///
/// Fails closed: propagates a git error instead of silently returning zero
/// added lines, which would make an unscannable file invisible to every
/// pattern-based check.
pub fn added_lines(cwd: &Path, file: &str) -> Result<Vec<String>, String> {
    let diff = file_diff(cwd, file)?;
    Ok(diff
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
        .filter(|l| !has_audit_ignore(l))
        .map(|l| l.to_string())
        .collect())
}

/// `git grep -l -E --untracked -- <pattern>`: files (tracked + untracked)
/// containing a line matching the regex.
///
/// Fails closed: a real git error (spawn failure or exit code other than 0/1)
/// propagates as `Err` rather than being conflated with "no match" (exit 1,
/// which legitimately yields an empty `Vec`).
pub fn grep_files(cwd: &Path, pattern: &str) -> Result<Vec<String>, String> {
    let args = &["grep", "-l", "-E", "--untracked", "--", pattern];
    let o = run_git(cwd, args)?;
    match o.code {
        Some(0) => Ok(o
            .stdout
            .lines()
            .map(|l| l.trim_end_matches('\r').to_string())
            .filter(|l| !l.is_empty())
            .collect()),
        Some(1) => Ok(Vec::new()), // exit 1 = no match (legitimately empty)
        _ => Err(exit_reason(args, &o)),
    }
}

/// `git rev-parse --show-toplevel`: the repo root, or None outside a repo.
/// (A not-a-repo non-zero exit OR spawn failure both yield None; the caller
/// falls back to cwd and the real fail-closed gate is `changed_and_untracked`.)
pub fn toplevel(cwd: &Path) -> Option<String> {
    let s = git_ok(cwd, &["rev-parse", "--show-toplevel"]).ok()?;
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// True if the line carries a reasoned `audit-ignore:` marker. A bare
/// `audit-ignore` without a following non-space char does NOT suppress.
pub fn has_audit_ignore(line: &str) -> bool {
    if let Some(idx) = line.find("audit-ignore:") {
        let rest = &line[idx + "audit-ignore:".len()..];
        rest.trim_start().chars().next().is_some()
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_and_untracked_fails_closed_outside_a_repo() {
        // A temp dir that is NOT a git repo: `git diff --name-only HEAD` exits
        // non-zero ("not a git repository"). The result MUST be Err, never Ok(empty)
        // — a git failure is an unknown working set, not a clean one.
        let dir = std::env::temp_dir().join(format!("pca-git-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let r = changed_and_untracked(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(r.is_err(), "git failure must fail closed, got {r:?}");
    }

    #[test]
    fn toplevel_is_none_outside_a_repo() {
        let dir = std::env::temp_dir().join(format!("pca-top-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let t = toplevel(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(t, None);
    }

    #[test]
    fn exit_reason_includes_code_and_stderr() {
        let o = GitOut {
            code: Some(128),
            stdout: String::new(),
            stderr: "fatal: bad\n".into(),
        };
        let r = exit_reason(&["diff", "HEAD"], &o);
        assert!(r.contains("128"), "{r}");
        assert!(r.contains("fatal: bad"), "{r}");
    }
}
