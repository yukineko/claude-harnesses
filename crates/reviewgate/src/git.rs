//! git helpers: which files changed, and the actual diff text for them. Pure
//! subprocess calls to `git`. `changed_files` returns a [`ChangeScan`]:
//! `NotRepo` means "not a git repo / git unavailable" (the caller then has no
//! diff to review and allows the stop), `Failed` means a git command errored
//! inside a real repo (the changeset is UNDETERMINED — the caller fails closed
//! and blocks rather than treat it as clean), and `Files(v)` is the
//! (possibly-empty) changed set.

use std::path::Path;
use std::process::Command;

/// Tri-state result of scanning the changed-files set. Distinguishes "no git
/// scope" (`NotRepo` → allow) from "a git command errored" (`Failed` →
/// undetermined → the gate fails closed / blocks), so a collapsed-empty scan
/// can never read as "nothing changed → allow". `Files(v)` is a success; an
/// EMPTY `Files` (a clean repo: `git diff` exits 0 with no output) is a
/// genuine no-changes → allow, NOT `Failed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeScan {
    NotRepo,
    Failed,
    Files(Vec<String>),
}

/// Changed paths relative to the repo root: tracked changes vs HEAD, staged
/// changes, and untracked-but-not-ignored files. Returns `NotRepo` when there
/// is no git repo, `Failed` when any sub-command errored (non-zero exit / spawn
/// failure) inside a real repo, or `Files(v)` on success (possibly empty).
pub fn changed_files(root: &Path) -> ChangeScan {
    if !is_git_repo(root) {
        return ChangeScan::NotRepo;
    }
    let mut out = Vec::new();
    // If ANY sub-command errors, the changed set is undetermined → fail closed.
    // A clean repo's commands all exit 0 with empty stdout → Files(vec![]).
    let ok = collect(root, &["diff", "--name-only"], &mut out)
        && collect(root, &["diff", "--cached", "--name-only"], &mut out)
        && collect(
            root,
            &["ls-files", "--others", "--exclude-standard"],
            &mut out,
        );
    if !ok {
        return ChangeScan::Failed;
    }
    out.sort();
    out.dedup();
    ChangeScan::Files(out)
}

fn is_git_repo(root: &Path) -> bool {
    Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run one `git` sub-command, appending its trimmed non-empty stdout lines to
/// `out`. Returns `true` on success (exit 0), `false` on a spawn error or a
/// non-zero exit — the caller maps `false` to `ChangeScan::Failed`. A
/// successful command with EMPTY stdout still returns `true` (clean ≠ failed).
fn collect(root: &Path, args: &[&str], out: &mut Vec<String>) -> bool {
    match Command::new("git").current_dir(root).args(args).output() {
        Ok(o) if o.status.success() => {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                let line = line.trim();
                if !line.is_empty() {
                    out.push(line.to_string());
                }
            }
            true
        }
        // Spawn error OR non-zero exit: the sub-command did not complete
        // successfully, so its (empty) output must not be trusted as "clean".
        _ => false,
    }
}

/// A reviewable diff plus whether it had to be truncated to fit `max_bytes`.
///
/// `truncated == true` means the real change was larger than `max_bytes` and the
/// tail was dropped. That dropped tail is **unreviewed**: neither the subprocess
/// reviewer (which only ever saw `text`) nor the inject-mode hash of `text` can
/// certify it. Callers must therefore never treat a truncated diff as a
/// complete, reviewed change — doing so would let the tail slip through the gate.
pub struct DiffText {
    pub text: String,
    pub truncated: bool,
}

/// The combined diff for `files`: unstaged + staged hunks, plus the full text of
/// any untracked files (which have no diff). Truncated to `max_bytes` with a
/// marker so a huge diff can't blow up memory or the reviewer prompt; the
/// returned `truncated` flag lets the caller refuse to silently allow a stop
/// whose tail was dropped.
pub fn diff_text(root: &Path, files: &[String], max_bytes: usize) -> DiffText {
    let mut s = String::new();

    run_diff(root, &["diff", "--"], files, &mut s);
    run_diff(root, &["diff", "--cached", "--"], files, &mut s);

    // untracked files among `files`: include their contents as "new file" diffs.
    let mut others = Vec::new();
    let mut args: Vec<&str> = vec!["ls-files", "--others", "--exclude-standard", "--"];
    args.extend(files.iter().map(String::as_str));
    if let Ok(o) = Command::new("git").current_dir(root).args(&args).output() {
        if o.status.success() {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                let line = line.trim();
                if !line.is_empty() {
                    others.push(line.to_string());
                }
            }
        }
    }
    for f in others {
        s.push_str(&format!("\n=== new file: {f} ===\n"));
        if let Ok(content) = std::fs::read_to_string(root.join(&f)) {
            s.push_str(&content);
            if !s.ends_with('\n') {
                s.push('\n');
            }
        }
        if s.len() > max_bytes {
            break;
        }
    }

    truncate_on_boundary(s, max_bytes)
}

fn run_diff(root: &Path, base: &[&str], files: &[String], out: &mut String) {
    let mut args: Vec<&str> = base.to_vec();
    args.extend(files.iter().map(String::as_str));
    if let Ok(o) = Command::new("git").current_dir(root).args(&args).output() {
        if o.status.success() {
            out.push_str(&String::from_utf8_lossy(&o.stdout));
        }
    }
}

fn truncate_on_boundary(mut s: String, max_bytes: usize) -> DiffText {
    if s.len() <= max_bytes {
        return DiffText {
            text: s,
            truncated: false,
        };
    }
    let mut cut = max_bytes;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s.push_str("\n… (diff truncated by reviewgate max_diff_bytes)\n");
    DiffText {
        text: s,
        truncated: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_limit_is_not_truncated() {
        let d = truncate_on_boundary("short diff".to_string(), 1000);
        assert!(!d.truncated);
        assert_eq!(d.text, "short diff");
    }

    #[test]
    fn over_limit_is_flagged_and_marked() {
        let big = "x".repeat(500);
        let d = truncate_on_boundary(big, 32);
        assert!(d.truncated, "a diff larger than max_bytes must be flagged");
        assert!(
            d.text.contains("truncated"),
            "the truncation marker must be present: {}",
            d.text
        );
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        // Each of these is 3 bytes; cutting at a byte that splits a char must
        // step back to a boundary rather than panic on truncate().
        let s = "あいうえお".to_string(); // 15 bytes
        let d = truncate_on_boundary(s, 7); // 7 is mid-character (6 is a boundary)
        assert!(d.truncated);
        // The head before the marker must remain valid UTF-8 (String guarantees
        // it, so merely constructing d without panicking proves the boundary
        // walk-back worked).
        assert!(d.text.starts_with("あい"));
    }

    // ── changed_files tri-state: NotRepo / Failed / Files ──────────────────

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// A fresh, unique scratch directory under the system temp dir (avoids a
    /// `tempfile` dev-dependency; mirrors this crate-family's existing test
    /// style). Caller is responsible for cleanup.
    fn scratch_dir() -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "reviewgate-git-test-{}-{}-{}",
            std::process::id(),
            line!(),
            n
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("create scratch dir");
        p
    }

    /// A directory that is not a git repo → `NotRepo` (no scope → allow),
    /// distinct from `Failed` (undetermined → block).
    #[test]
    fn non_repo_dir_is_notrepo() {
        let root = scratch_dir();
        assert_eq!(changed_files(&root), ChangeScan::NotRepo);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A clean repo is a SUCCESSFUL empty scan → `Files(vec![])`, never
    /// `Failed`. This pins that a clean tree (git diff exits 0 with empty
    /// stdout) does not start blocking under the fail-closed change.
    #[test]
    fn clean_repo_is_empty_files_not_failed() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = scratch_dir();
        let root = root.as_path();
        for args in [
            &["init", "-q"][..],
            &["config", "user.email", "t@t.com"][..],
            &["config", "user.name", "t"][..],
        ] {
            assert!(Command::new("git")
                .current_dir(root)
                .args(args)
                .status()
                .expect("git")
                .success());
        }
        std::fs::write(root.join("a.txt"), "x\n").unwrap();
        assert!(Command::new("git")
            .current_dir(root)
            .args(["add", "a.txt"])
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .current_dir(root)
            .args(["commit", "-qm", "init"])
            .status()
            .unwrap()
            .success());

        assert_eq!(
            changed_files(root),
            ChangeScan::Files(Vec::new()),
            "a clean repo must be a successful empty scan, not Failed"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// The `collect` helper's success/failure signal (which drives the Failed
    /// mapping) must distinguish an errored sub-command from an empty-but-
    /// successful one: a bogus git flag exits non-zero → `false`; a valid
    /// command with no output → `true`.
    #[test]
    fn collect_reports_error_vs_empty_success() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = scratch_dir();
        let root = root.as_path();
        assert!(Command::new("git")
            .current_dir(root)
            .args(["init", "-q"])
            .status()
            .expect("git")
            .success());

        let mut out = Vec::new();
        // Valid command, no changes → success (true), even with empty output.
        assert!(
            collect(root, &["diff", "--name-only"], &mut out),
            "an empty-but-successful git command must report true (not Failed)"
        );
        assert!(out.is_empty());
        // Bogus flag → non-zero exit → false → caller maps to Failed.
        assert!(
            !collect(root, &["diff", "--no-such-flag-xyzzy"], &mut out),
            "a non-zero git exit must report false so the scan fails closed"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
