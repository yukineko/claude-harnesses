//! Pass cache (backlog e04711be): a check that PASSED on a working tree is not
//! re-executed while nothing that could change its outcome has changed.
//!
//! Fail-closed contract (CLAUDE.md §3) — the cache can only ever SKIP a re-run of
//! a check that already passed on byte-identical inputs; it can never turn a
//! failure into a pass:
//!
//! * Only passes are stored. A failed run removes the entry for that check.
//! * The key is the root path, the check name, the exact `cmd` string, its
//!   `workdir` and effective timeout, `HEAD`'s sha, the sha `main` resolves to
//!   (or a determined "absent") — together these fix the merge-base donegate's
//!   changed set is computed against — and the working-tree content.
//! * Working-tree content is a git tree object built in a TEMPORARY index
//!   (`GIT_INDEX_FILE` under `<state_dir>/tmp`, started empty so no
//!   assume-unchanged / skip-worktree bit or stale stat data is inherited):
//!   `git add -A` (tracked + untracked-not-ignored), then a forced add of the
//!   paths the real index tracks although they are ignored, then
//!   `git write-tree`. The real index is only ever read (`git ls-files`).
//!   Side effect: blobs for the hashed files are written to the object store.
//! * Any failure computing the key (not a repo, git error, unreadable file) or
//!   reading / parsing the store, or a stored key that differs in any byte, is a
//!   MISS: the check runs.
//! * The tree is computed before and after the checks run; passes are stored
//!   only if both agree (a check that rewrote the tree, or a concurrent edit,
//!   stores nothing).
//!
//! Not covered by the key: files ignored by git and not tracked (e.g. `target/`),
//! files outside the repository, and the environment the check inherits.
//!
//! A hit is never silent: the outcome is marked `cached`, rendered as a cached
//! pass in the human report, logged as a `cache-hit` event and announced on
//! stderr.
//!
//! Store: `<state_dir>/pass-cache/<hash(root, check name)>.json`, one entry per
//! (root, check), overwritten by the latest pass.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::config::Check;

/// Format version of a stored entry; a different value is a miss.
const STORE_VERSION: u64 = 1;

static SEQ: AtomicU32 = AtomicU32::new(0);

/// A computed snapshot of the repository state the key depends on.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct TreeState {
    head: String,
    main: String,
    tree: String,
}

impl TreeState {
    /// The full state as one exact string (HEAD, `main`, working-tree tree),
    /// for keys outside this module (`duptest`).
    pub(crate) fn fingerprint(&self) -> String {
        serde_json::json!({ "head": self.head, "main": self.main, "tree": self.tree }).to_string()
    }
}

fn git(root: &Path) -> Command {
    let mut c = Command::new("git");
    harness_core::git_env::scrub(&mut c);
    c.current_dir(root);
    c
}

/// stdout of a git command that exited 0, trimmed; `None` on anything else.
fn git_ok(cmd: &mut Command) -> Option<String> {
    let o = cmd.stderr(Stdio::null()).output().ok()?;
    if !o.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// `rev-parse --verify -q <rev>`: `Some(Some(sha))` resolved, `Some(None)` a
/// determined "does not exist" (exit 1), `None` undetermined.
fn resolve(root: &Path, rev: &str) -> Option<Option<String>> {
    let o = git(root)
        .args(["rev-parse", "--verify", "-q", rev])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if o.status.success() {
        let sha = String::from_utf8_lossy(&o.stdout).trim().to_string();
        return if sha.is_empty() {
            None
        } else {
            Some(Some(sha))
        };
    }
    if o.status.code() == Some(1) {
        return Some(None);
    }
    None
}

/// The repository state for the key, or `None` (= MISS) when any part of it
/// cannot be determined.
pub(crate) fn tree_state(root: &Path, tmp_dir: &Path) -> Option<TreeState> {
    match harness_core::git_probe::probe_repo_scrubbed(root) {
        harness_core::git_probe::RepoProbe::Repo => {}
        _ => return None,
    }
    let head = resolve(root, "HEAD^{commit}")?.unwrap_or_else(|| "<unborn>".to_string());
    let main = resolve(root, "main^{commit}")?.unwrap_or_else(|| "<absent>".to_string());
    std::fs::create_dir_all(tmp_dir).ok()?;
    let index = tmp_dir.join(format!(
        "passcache-index-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&index);
    let tree = build_tree(root, &index);
    let _ = std::fs::remove_file(&index);
    Some(TreeState {
        head,
        main,
        tree: tree?,
    })
}

fn build_tree(root: &Path, index: &Path) -> Option<String> {
    // Tracked + untracked-not-ignored, from the repository top level.
    git_ok(
        git(root)
            .env("GIT_INDEX_FILE", index)
            .args(["add", "-A", "--", ":/"])
            .stdout(Stdio::null()),
    )?;
    // Paths the REAL index tracks although they match an ignore rule: `add -A`
    // into an empty index would not see them. Reading the real index only.
    let o = git(root)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--ignored",
            "--exclude-standard",
            "--full-name",
        ])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !o.status.success() {
        return None;
    }
    if !o.stdout.is_empty() {
        let top = git_ok(git(root).args(["rev-parse", "--show-toplevel"]))?;
        let mut child = git(Path::new(&top))
            .env("GIT_INDEX_FILE", index)
            .args(["add", "-f", "--pathspec-from-file=-", "--pathspec-file-nul"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        {
            use std::io::Write;
            let mut stdin = child.stdin.take()?;
            stdin.write_all(&o.stdout).ok()?;
        }
        if !child.wait().ok()?.success() {
            return None;
        }
    }
    let tree = git_ok(git(root).env("GIT_INDEX_FILE", index).arg("write-tree"))?;
    if tree.is_empty() {
        return None;
    }
    Some(tree)
}

/// The exact key string for `check` on `root` in `state`.
fn key(root: &Path, check: &Check, timeout: u64, state: &TreeState) -> String {
    serde_json::json!({
        "root": root.to_string_lossy(),
        "name": check.name,
        "cmd": check.cmd,
        "workdir": check.workdir,
        "timeout_secs": timeout,
        "head": state.head,
        "main": state.main,
        "tree": state.tree,
    })
    .to_string()
}

fn entry_path(state_dir: &Path, root: &Path, name: &str) -> PathBuf {
    let mut h = DefaultHasher::new();
    root.hash(&mut h);
    name.hash(&mut h);
    state_dir
        .join("pass-cache")
        .join(format!("{:016x}.json", h.finish()))
}

/// When the stored pass was recorded, if the store holds a pass for exactly
/// this key. Every read / parse failure or mismatch is `None` (MISS).
pub(crate) fn lookup(
    state_dir: &Path,
    root: &Path,
    check: &Check,
    timeout: u64,
    state: &TreeState,
) -> Option<String> {
    let raw = std::fs::read(entry_path(state_dir, root, &check.name)).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    if v.get("v")?.as_u64()? != STORE_VERSION {
        return None;
    }
    if !v.get("passed")?.as_bool()? {
        return None;
    }
    if v.get("key")?.as_str()? != key(root, check, timeout, state) {
        return None;
    }
    Some(v.get("passed_at")?.as_str()?.to_string())
}

/// Record a pass. A failure to write leaves no (or the previous) entry — at
/// worst a later MISS, never a false hit, since a hit needs the exact key.
pub(crate) fn store(state_dir: &Path, root: &Path, check: &Check, timeout: u64, state: &TreeState) {
    let path = entry_path(state_dir, root, &check.name);
    let Some(dir) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let body = serde_json::json!({
        "v": STORE_VERSION,
        "passed": true,
        "key": key(root, check, timeout, state),
        "passed_at": chrono::Local::now().to_rfc3339(),
    })
    .to_string();
    let tmp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    if std::fs::write(&tmp, body).is_err() || std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Drop the entry for (root, check): called when the check failed.
pub(crate) fn forget(state_dir: &Path, root: &Path, name: &str) {
    let _ = std::fs::remove_file(entry_path(state_dir, root, name));
}
