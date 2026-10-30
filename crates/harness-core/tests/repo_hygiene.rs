//! Repo-hygiene regression test: runtime/ephemeral state must never be tracked.
//!
//! The repo `.gitignore` excludes machine-local runtime artifacts — compass carve
//! state, compass outcomes, the precommit-audit audit log, session progress, build
//! output, Python bytecode. Ignoring them once is not enough: a careless
//! `git add -f`, a rewritten `.gitignore`, or a tool that writes into a tracked
//! path could quietly start committing them again, leaking per-machine state into
//! everyone's checkout. This test fails loudly if any such file is in the index.
//!
//! Ported from AEGIS `tests/repo_hygiene/test_no_tracked_runtime_db.py` (which
//! asserts `git ls-files` is empty for its runtime DB patterns).

use std::path::{Path, PathBuf};
use std::process::Command;

/// Nearest ancestor of `start` containing `.git`, or None outside a checkout.
fn repo_root(start: &Path) -> Option<PathBuf> {
    let mut cur = start.to_path_buf();
    loop {
        if cur.join(".git").exists() {
            return Some(cur);
        }
        if !cur.pop() {
            return None;
        }
    }
}

/// True if `tracked` (a repo-relative path from `git ls-files`) is a runtime
/// artifact that must stay untracked. Mirrors the `.gitignore` runtime entries.
fn is_runtime_artifact(tracked: &str) -> bool {
    // Build output and editor/OS scratch.
    if tracked == "target" || tracked.starts_with("target/") || tracked.contains("/target/") {
        return true;
    }
    if tracked.ends_with(".rs.bk") || tracked.ends_with("Cargo.lock.bak") || tracked == ".DS_Store"
    {
        return true;
    }
    // Python bytecode cache.
    if tracked.ends_with(".pyc") || tracked.contains("__pycache__/") {
        return true;
    }
    // Ephemeral session/tool state. Match on the trailing path so a copy under
    // any directory (e.g. a nested project root) is still caught.
    const EPHEMERAL_SUFFIXES: [&str; 4] = [
        ".claude/audit-log.jsonl",
        ".claude/progress.md",
        ".compass/carve-state.json",
        ".compass/outcomes.json",
    ];
    EPHEMERAL_SUFFIXES
        .iter()
        .any(|suf| tracked == *suf || tracked.ends_with(&format!("/{suf}")))
}

#[test]
fn no_runtime_artifacts_are_tracked() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let Some(root) = repo_root(&manifest_dir) else {
        // Built outside a git checkout (e.g. a packaged crate) — nothing to assert.
        eprintln!("skipping: not inside a git checkout");
        return;
    };

    let out = match harness_core::git_env::scrub(&mut Command::new("git"))
        .current_dir(&root)
        .args(["ls-files", "--cached"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            eprintln!(
                "skipping: `git ls-files` failed: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            );
            return;
        }
        Err(e) => {
            eprintln!("skipping: could not run git: {e}");
            return;
        }
    };

    let listing = String::from_utf8_lossy(&out.stdout);
    let offenders: Vec<&str> = listing
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter(|l| is_runtime_artifact(l))
        .collect();

    assert!(
        offenders.is_empty(),
        "runtime/ephemeral artifacts must never be tracked, but `git ls-files` \
         lists these (add them to .gitignore and `git rm --cached`): {offenders:#?}"
    );
}

#[test]
fn pattern_matcher_recognises_known_runtime_paths() {
    // Guard the matcher itself so a future refactor can't quietly let an
    // artifact through by breaking the predicate.
    for p in [
        "target/debug/foo",
        "crates/x/target/release/bar",
        ".compass/carve-state.json",
        ".compass/outcomes.json",
        ".claude/audit-log.jsonl",
        ".claude/progress.md",
        "scripts/__pycache__/x.cpython-312.pyc",
        "x.pyc",
        "src/lib.rs.bk",
    ] {
        assert!(is_runtime_artifact(p), "should flag runtime artifact: {p}");
    }
    for p in [
        "crates/harness-core/src/lib.rs",
        "README.md",
        ".gitignore",
        ".compass/charter.md", // charter IS tracked — only carve/outcomes are runtime
        "targeting/notes.md",  // not the build dir
    ] {
        assert!(
            !is_runtime_artifact(p),
            "should NOT flag source/tracked: {p}"
        );
    }
}
