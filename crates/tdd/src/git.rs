//! Talk to `git` to learn what changed: the set of changed paths (for test-file
//! evidence) and the *added* lines with their file (for inline-test detection
//! and counting newly added implementation lines). Pure subprocess calls.

use std::path::Path;
use std::process::Command;

/// One added line in the working tree's diff, tagged with the file it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddedLine {
    pub file: String,
    pub text: String,
}

/// Tri-state result of scanning the changed-files set. Distinguishes "no git
/// scope" from "a git command errored", so the gate can fail *closed* on the
/// latter instead of collapsing both into an empty "nothing changed → allow".
///
/// * `NotRepo`  — not a git repo → the gate has no scope → allow (unchanged).
/// * `Failed`   — in a real repo, but a `git` command errored (non-zero exit /
///   spawn failure) → the changed set is UNDETERMINED → the gate must fail
///   closed (block), never treat undetermined as clean.
/// * `Files(v)` — success; the (possibly EMPTY) changed set. Empty = genuinely
///   clean → allow path unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeScan {
    NotRepo,
    Failed,
    Files(Vec<String>),
}

/// Tri-state result of scanning the *added* lines. Same NotRepo / Failed /
/// success trichotomy as [`ChangeScan`]: a failed git *command* (a failed
/// `git diff -U0` / `git diff --cached -U0` / `git ls-files --others`) is
/// `Failed` (undetermined → fail closed). Individual untracked-file *read*
/// errors stay best-effort (skipped), since a git command that succeeded
/// listed the file — only a failed git command is `Failed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddedScan {
    NotRepo,
    Failed,
    Lines(Vec<AddedLine>),
}

fn is_git_repo(root: &Path) -> bool {
    Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run(root: &Path, args: &[&str]) -> Option<String> {
    let o = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .ok()?;
    if o.status.success() {
        Some(String::from_utf8_lossy(&o.stdout).into_owned())
    } else {
        None
    }
}

/// Changed paths relative to the repo root: tracked changes vs HEAD, staged
/// changes, and untracked-but-not-ignored files. Returns a [`ChangeScan`]:
/// `NotRepo` (no git scope), `Failed` (a git command errored in a real repo —
/// undetermined, gate fails closed), or `Files(v)` (the possibly-empty changed
/// set). A clean repo yields `Files(vec![])` (the `git status` call exits 0
/// with empty stdout), NOT `Failed`.
///
/// Implemented as a single `git status --porcelain=v1 -z` spawn (instead of
/// three separate `git diff` / `git diff --cached` / `git ls-files` calls).
/// `-z` gives NUL-terminated, unquoted paths (avoiding the C-style quoting
/// that plain `--porcelain` applies to paths with spaces/special chars), and
/// porcelain v1's `XY PATH` records cover unstaged, staged, and untracked
/// state in one pass. Renames/copies emit an extra NUL-separated "orig path"
/// field ahead of the (new) path in the record stream; we keep only the new
/// path, matching what `git diff --name-only` reports for a rename.
pub fn changed_files(root: &Path) -> ChangeScan {
    if !is_git_repo(root) {
        return ChangeScan::NotRepo;
    }
    // A failed `git status` (non-zero exit / spawn error) inside a real repo is
    // an UNDETERMINED changeset, not an empty one — fail closed. A clean repo
    // succeeds with empty stdout and flows through to `Files(vec![])`.
    let Some(raw) = run(root, &["status", "--porcelain=v1", "-z"]) else {
        return ChangeScan::Failed;
    };
    let mut out = Vec::new();
    let mut tokens = raw.split('\0').peekable();
    while let Some(record) = tokens.next() {
        if record.is_empty() {
            continue;
        }
        // Record format: "XY PATH" (status codes are always 2 chars + space).
        if record.len() < 3 {
            continue;
        }
        let (status, path) = record.split_at(2);
        let path = &path[1..]; // drop the single space separator
                               // Renames/copies ('R'/'C' in either the staged-X or worktree-Y column)
                               // carry an extra orig-path field as the *next* NUL-separated token;
                               // consume and discard it so it isn't mistaken for the next record.
        if status.contains('R') || status.contains('C') {
            tokens.next();
        }
        if !path.is_empty() {
            out.push(path.to_string());
        }
    }
    out.sort();
    out.dedup();
    ChangeScan::Files(out)
}

/// All *added* lines across unstaged + staged diffs, plus the full content of
/// untracked files (which have no diff). Returns an [`AddedScan`]: `NotRepo`,
/// `Failed` (a git command errored → undetermined → gate fails closed), or
/// `Lines(v)` (the possibly-empty added-line set). A failed git *command* is
/// `Failed`; an individual untracked-file *read* error is best-effort (skipped).
pub fn added_lines(root: &Path) -> AddedScan {
    if !is_git_repo(root) {
        return AddedScan::NotRepo;
    }
    let mut out = Vec::new();
    for args in [&["diff", "-U0"][..], &["diff", "--cached", "-U0"][..]] {
        // A failed diff command is undetermined, not "no added lines".
        let Some(text) = run(root, args) else {
            return AddedScan::Failed;
        };
        parse_unified_diff(&text, &mut out);
    }
    // Untracked files: every line is "added". A failed `ls-files` is undetermined.
    let Some(text) = run(root, &["ls-files", "--others", "--exclude-standard"]) else {
        return AddedScan::Failed;
    };
    for f in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        // Individual file-read errors stay best-effort: git already listed the
        // file, so this is not a git-command failure — skip the unreadable file.
        if let Ok(content) = std::fs::read_to_string(root.join(f)) {
            for line in content.lines() {
                out.push(AddedLine {
                    file: f.to_string(),
                    text: line.to_string(),
                });
            }
        }
    }
    AddedScan::Lines(out)
}

/// Parse `git diff` unified output, collecting `+` lines tagged with the file
/// from the most recent `+++ b/<path>` header. `+++`/`---` markers are skipped.
pub fn parse_unified_diff(diff: &str, out: &mut Vec<AddedLine>) {
    let mut current: Option<String> = None;
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            current = Some(strip_diff_prefix(rest));
            continue;
        }
        if line.starts_with("--- ") || line.starts_with("+++") {
            continue;
        }
        if let Some(added) = line.strip_prefix('+') {
            if let Some(file) = &current {
                if file != "/dev/null" {
                    out.push(AddedLine {
                        file: file.clone(),
                        text: added.to_string(),
                    });
                }
            }
        }
    }
}

/// `b/src/main.rs` → `src/main.rs`; `/dev/null` stays as-is.
fn strip_diff_prefix(path: &str) -> String {
    let p = path.trim();
    if p == "/dev/null" {
        return p.to_string();
    }
    p.strip_prefix("a/")
        .or_else(|| p.strip_prefix("b/"))
        .unwrap_or(p)
        .to_string()
}

/// Reference implementation kept ONLY for regression tests: reproduces the
/// original 3-subprocess-spawn logic (`git diff --name-only`, `git diff
/// --cached --name-only`, `git ls-files --others --exclude-standard`) that
/// `changed_files` used before it was batched into a single `git status`
/// call. Tests assert the batched path yields an identical set.
#[cfg(test)]
fn changed_files_legacy(root: &Path) -> Option<Vec<String>> {
    if !is_git_repo(root) {
        return None;
    }
    let mut out = Vec::new();
    for args in [
        &["diff", "--name-only"][..],
        &["diff", "--cached", "--name-only"][..],
        &["ls-files", "--others", "--exclude-standard"][..],
    ] {
        if let Some(text) = run(root, args) {
            for line in text.lines() {
                let line = line.trim();
                if !line.is_empty() {
                    out.push(line.to_string());
                }
            }
        }
    }
    out.sort();
    out.dedup();
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_added_lines_with_file() {
        let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -0,0 +1,2 @@
+pub fn add(a: i32, b: i32) -> i32 { a + b }
+// note
diff --git a/old.rs b/old.rs
--- a/old.rs
+++ /dev/null
@@ -1 +0,0 @@
-gone
";
        let mut out = Vec::new();
        parse_unified_diff(diff, &mut out);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|l| l.file == "src/lib.rs"));
        assert!(out[0].text.contains("pub fn add"));
    }

    // --- changed_files batching regression: the single `git status
    // --porcelain=v1 -z` spawn must yield the exact same set as the original
    // 3-call (diff / diff --cached / ls-files --others) implementation,
    // across modified, staged, untracked, renamed, deleted, and mixed
    // staged+modified files. These need a real `git` binary and a scratch
    // repo; skip gracefully (rather than fail) if `git` isn't runnable in
    // the sandbox, matching this repo's convention of not hard-failing CI on
    // missing external tooling.

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempfile::tempdir");
        let root = dir.path();
        for args in [
            &["init", "-q"][..],
            &["config", "user.email", "t@t.com"][..],
            &["config", "user.name", "t"][..],
        ] {
            let status = Command::new("git")
                .current_dir(root)
                .args(args)
                .status()
                .expect("git init/config");
            assert!(status.success(), "git {args:?} should succeed");
        }
        dir
    }

    fn write(root: &Path, rel: &str, contents: &str) {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("create_dir_all");
        }
        std::fs::write(p, contents).expect("write");
    }

    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(root)
            .args(args)
            .status()
            .unwrap_or_else(|e| panic!("git {args:?} failed to spawn: {e}"));
        assert!(status.success(), "git {args:?} should succeed");
    }

    /// Unwrap a `ChangeScan` to its file vector, panicking on NotRepo/Failed.
    /// Used by the batching-regression tests, which always run in a real repo.
    fn files_of(scan: ChangeScan) -> Vec<String> {
        match scan {
            ChangeScan::Files(v) => v,
            other => panic!("expected ChangeScan::Files, got {other:?}"),
        }
    }

    /// Assert the batched `changed_files` matches the legacy 3-call
    /// implementation, for whatever state the given repo is currently in.
    fn assert_sets_match(root: &Path, scenario: &str) {
        let batched = files_of(changed_files(root));
        let legacy = changed_files_legacy(root).unwrap_or_default();
        assert_eq!(
            batched, legacy,
            "changed_files set diverged from legacy 3-call implementation in scenario: {scenario}"
        );
    }

    #[test]
    fn changed_files_matches_legacy_modified_staged_untracked() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let dir = init_repo();
        let root = dir.path();

        write(root, "a.txt", "hello\n");
        write(root, "b.txt", "hello\n");
        git(root, &["add", "a.txt", "b.txt"]);
        git(root, &["commit", "-qm", "init"]);

        // Working-tree modification (unstaged).
        write(root, "a.txt", "hello\nmodified\n");
        // Staged modification.
        write(root, "b.txt", "hello\nstaged\n");
        git(root, &["add", "b.txt"]);
        // Untracked-but-not-ignored file.
        write(root, "c.txt", "new\n");

        assert_sets_match(root, "modified+staged+untracked");
        let files = files_of(changed_files(root));
        assert_eq!(files, vec!["a.txt", "b.txt", "c.txt"]);
    }

    #[test]
    fn changed_files_matches_legacy_staged_plus_further_modified() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let dir = init_repo();
        let root = dir.path();

        write(root, "d.txt", "base\n");
        git(root, &["add", "d.txt"]);
        git(root, &["commit", "-qm", "init"]);

        // Stage a change, then modify again on top (status "MM").
        write(root, "d.txt", "base\nstaged\n");
        git(root, &["add", "d.txt"]);
        write(root, "d.txt", "base\nstaged\nmodified-again\n");

        assert_sets_match(root, "staged+modified (MM)");
        let files = files_of(changed_files(root));
        assert_eq!(files, vec!["d.txt"]);
    }

    #[test]
    fn changed_files_matches_legacy_renamed_staged() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let dir = init_repo();
        let root = dir.path();

        write(root, "old.txt", "content\n");
        git(root, &["add", "old.txt"]);
        git(root, &["commit", "-qm", "init"]);

        git(root, &["mv", "old.txt", "new.txt"]);

        assert_sets_match(root, "renamed (staged, pure)");
        let files = files_of(changed_files(root));
        assert_eq!(files, vec!["new.txt"]);
    }

    #[test]
    fn changed_files_matches_legacy_renamed_and_modified() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let dir = init_repo();
        let root = dir.path();

        write(root, "old.txt", "content\nmore\n");
        git(root, &["add", "old.txt"]);
        git(root, &["commit", "-qm", "init"]);

        git(root, &["mv", "old.txt", "new.txt"]);
        write(root, "new.txt", "content\nmore\nextra\n");

        assert_sets_match(root, "renamed+modified (RM)");
        let files = files_of(changed_files(root));
        assert_eq!(files, vec!["new.txt"]);
    }

    #[test]
    fn changed_files_matches_legacy_deleted_unstaged_then_staged() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let dir = init_repo();
        let root = dir.path();

        write(root, "e.txt", "gone-soon\n");
        write(root, "f.txt", "also-gone\n");
        git(root, &["add", "e.txt", "f.txt"]);
        git(root, &["commit", "-qm", "init"]);

        // Unstaged deletion.
        std::fs::remove_file(root.join("e.txt")).expect("remove_file");
        assert_sets_match(root, "deleted (unstaged)");
        let files = files_of(changed_files(root));
        assert_eq!(files, vec!["e.txt"]);

        // Stage the deletion (f.txt was never touched, so it stays absent
        // from the changed set).
        git(root, &["add", "-A"]);
        assert_sets_match(root, "deleted (staged)");
        let files = files_of(changed_files(root));
        assert_eq!(files, vec!["e.txt"]);
    }

    #[test]
    fn changed_files_matches_legacy_path_with_space() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let dir = init_repo();
        let root = dir.path();

        write(root, "base.txt", "base\n");
        git(root, &["add", "base.txt"]);
        git(root, &["commit", "-qm", "init"]);

        write(root, "space file.txt", "untracked with space\n");

        assert_sets_match(root, "untracked path with space");
        let files = files_of(changed_files(root));
        assert_eq!(files, vec!["space file.txt"]);
    }

    #[test]
    fn changed_files_matches_legacy_clean_repo() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let dir = init_repo();
        let root = dir.path();
        write(root, "only.txt", "content\n");
        git(root, &["add", "only.txt"]);
        git(root, &["commit", "-qm", "init"]);

        assert_sets_match(root, "clean repo, no changes");
        // A clean repo is a SUCCESSFUL empty scan — `Files(vec![])`, never
        // `Failed`. This pins that clean repos don't start reporting Failed
        // (which would fail the gate closed on a genuinely-clean tree).
        assert_eq!(changed_files(root), ChangeScan::Files(Vec::new()));
        assert!(matches!(added_lines(root), AddedScan::Lines(v) if v.is_empty()));
    }

    /// A directory that is not a git repo maps to `NotRepo` (no scope → allow),
    /// distinct from the `Failed` (undetermined → block) path. Doesn't need a
    /// real git binary beyond `is_git_repo` returning false.
    #[test]
    fn non_repo_dir_maps_to_notrepo() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(changed_files(dir.path()), ChangeScan::NotRepo);
        assert_eq!(added_lines(dir.path()), AddedScan::NotRepo);
    }

    /// A non-zero `git` exit (a real repo, but the command errors) must surface
    /// as `Failed`, while a successful empty-stdout command is `Files(vec![])`.
    /// This is the git.rs-level pin that distinguishes error from clean, which
    /// the whole fail-closed fix rests on. Exercised through `run` (the shared
    /// per-command helper): a bogus subcommand exits non-zero → `None`, an
    /// empty-but-successful `status` → `Some("")`.
    #[test]
    fn nonzero_git_exit_is_none_empty_success_is_some() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let dir = init_repo();
        let root = dir.path();
        write(root, "only.txt", "content\n");
        git(root, &["add", "only.txt"]);
        git(root, &["commit", "-qm", "init"]);

        // Successful command on a clean tree: empty stdout, but SUCCESS.
        assert_eq!(
            run(root, &["status", "--porcelain=v1", "-z"]),
            Some(String::new()),
            "a clean `git status` exits 0 with empty stdout → Some(\"\"), i.e. success"
        );
        // A command that errors (non-zero exit) → None, which callers map to Failed.
        assert_eq!(
            run(root, &["diff", "--no-such-flag-xyzzy"]),
            None,
            "a non-zero git exit must be None (→ Failed), never mistaken for empty success"
        );
    }
}
