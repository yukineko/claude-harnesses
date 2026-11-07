//! Pass cache for the mechanical done_criteria command run by
//! `condukt state check-criteria` (backlog ebb0e1b5).
//!
//! A command that PASSED against a task worktree is not re-executed while the
//! exact argv and the worktree content are unchanged.
//!
//! * **Key** = exact argv + the git tree id of the worktree content (tracked
//!   plus untracked-not-ignored files). The tree id is computed with
//!   `git add -A` into a throwaway `GIT_INDEX_FILE` followed by
//!   `git write-tree`; the real index is never written.
//! * **Store** = one JSON file per key under `<state_dir>/pass-cache/`
//!   (`~/.condukt/state/pass-cache/` by default). Each file carries the full
//!   key material (argv + tree), and a hit requires an exact match on both,
//!   so a file-name hash collision is a miss, never a false hit.
//! * Only passes are stored. A failed run is never cached.
//!
//! Fail-closed (CLAUDE.md §3): any failure to compute the key (not a git
//! repo, git exits non-zero, a submodule/gitlink whose dirty content the tree
//! id would not capture, temp-file IO), or to read or parse a stored entry, is
//! a MISS — the command runs. Writing the entry is best-effort: a failed write
//! only means the next check runs the command again.
//!
//! Known limit, by the pinned contract: ignored files (build output, `.env`)
//! and anything outside the worktree are not part of the key.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Store schema tag; a file with any other value is a miss.
const SCHEMA: &str = "condukt-pass-cache/1";

/// The cache key: the exact argv and the worktree content tree id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub argv: Vec<String>,
    pub tree: String,
}

/// Directory holding the cache entries.
pub fn store_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("pass-cache")
}

fn git(dir: &Path) -> Command {
    let mut c = Command::new("git");
    harness_core::git_env::scrub(&mut c);
    c.current_dir(dir);
    c
}

/// Compute the content tree id of `dir` without touching its real index.
/// `None` = could not determine (the caller then runs the command).
pub fn worktree_tree(dir: &Path) -> Option<String> {
    let top = git(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !top.status.success() {
        return None;
    }
    let top = PathBuf::from(String::from_utf8(top.stdout).ok()?.trim());
    if top.as_os_str().is_empty() {
        return None;
    }
    let tmp = tempfile::Builder::new()
        .prefix("condukt-pass-cache-index-")
        .tempdir()
        .ok()?;
    let index = tmp.path().join("index");
    let add = git(&top)
        .env("GIT_INDEX_FILE", &index)
        .args(["add", "-A", "--", "."])
        .output()
        .ok()?;
    if !add.status.success() {
        return None;
    }
    // A gitlink (submodule / nested repo) records only its HEAD commit, not
    // its working content, so the tree id would not see edits inside it.
    let staged = git(&top)
        .env("GIT_INDEX_FILE", &index)
        .args(["ls-files", "-s"])
        .output()
        .ok()?;
    if !staged.status.success() {
        return None;
    }
    if String::from_utf8_lossy(&staged.stdout)
        .lines()
        .any(|l| l.starts_with("160000 "))
    {
        return None;
    }
    let wt = git(&top)
        .env("GIT_INDEX_FILE", &index)
        .arg("write-tree")
        .output()
        .ok()?;
    if !wt.status.success() {
        return None;
    }
    let tree = String::from_utf8(wt.stdout).ok()?.trim().to_string();
    if tree.is_empty() || !tree.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(tree)
}

/// The key for running `argv` in `dir`, or `None` when it cannot be computed.
pub fn key_for(argv: &[String], dir: &Path) -> Option<Key> {
    if argv.is_empty() {
        return None;
    }
    Some(Key {
        argv: argv.to_vec(),
        tree: worktree_tree(dir)?,
    })
}

fn entry_path(store: &Path, key: &Key) -> Option<PathBuf> {
    let material = serde_json::to_string(&serde_json::json!({
        "argv": key.argv,
        "tree": key.tree,
    }))
    .ok()?;
    let h = harness_core::hash::fnv1a64(material.as_bytes());
    Some(store.join(format!("{h:016x}.json")))
}

/// The recorded output of a cached pass, or `None` (= miss) when there is no
/// entry, it cannot be read or parsed, or its key material does not match
/// `key` exactly.
pub fn lookup(store: &Path, key: &Key) -> Option<String> {
    let raw = std::fs::read(entry_path(store, key)?).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    if v.get("schema")?.as_str()? != SCHEMA {
        return None;
    }
    if !v.get("passed")?.as_bool()? {
        return None;
    }
    if v.get("tree")?.as_str()? != key.tree {
        return None;
    }
    let argv: Vec<String> = serde_json::from_value(v.get("argv")?.clone()).ok()?;
    if argv != key.argv {
        return None;
    }
    Some(v.get("output")?.as_str()?.to_string())
}

/// Record a PASS for `key`. Best-effort; returns whether the entry landed.
pub fn record_pass(store: &Path, key: &Key, output: &str) -> bool {
    let Some(path) = entry_path(store, key) else {
        return false;
    };
    if std::fs::create_dir_all(store).is_err() {
        return false;
    }
    let body = serde_json::json!({
        "schema": SCHEMA,
        "passed": true,
        "argv": key.argv,
        "tree": key.tree,
        "output": output,
    });
    let Ok(bytes) = serde_json::to_vec(&body) else {
        return false;
    };
    let Ok(mut tmp) = tempfile::NamedTempFile::new_in(store) else {
        return false;
    };
    if std::io::Write::write_all(&mut tmp, &bytes).is_err() {
        return false;
    }
    tmp.persist(&path).is_ok()
}

/// Run `argv` in `dir` through the pass cache. `run` executes the command and
/// returns `(passed, output)`. Returns `(passed, output, cache_hit)`; on a hit
/// the command is not executed and the output is the recorded one, prefixed
/// with a visible cache-hit marker.
pub fn run_cached(
    store: &Path,
    argv: &[String],
    dir: &Path,
    run: impl FnOnce(&[String], &Path) -> (bool, String),
) -> (bool, String, bool) {
    let key = key_for(argv, dir);
    if let Some(k) = &key {
        if let Some(prev) = lookup(store, k) {
            let out = format!(
                "[condukt pass-cache HIT: not re-executed; same argv on unchanged worktree content (tree {})]\n{prev}",
                k.tree
            );
            return (true, out, true);
        }
    }
    let (passed, output) = run(argv, dir);
    if passed {
        if let Some(k) = &key {
            let _ = record_pass(store, k, &output);
        }
    }
    (passed, output, false)
}
