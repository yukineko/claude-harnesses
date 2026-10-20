//! `mode = "loose"`: warn once per file hash (see [`crate::config::Mode`]).
//!
//! Operator ruling 2026-10-20: 「strict は毎回警告するが、そうでないなら一つの
//! file の hash にたいして一度だけ」. The unit is one implementation file at
//! one content: an entry is `<path>\t<hash of its bytes>`. A finding is shown
//! when ANY file it names is at a (path, hash) not shown before; when every
//! named file is at an already-shown (path, hash), the stop goes through
//! without repeating it. Editing a file gives it a new hash, so it is shown
//! again.
//!
//! The ledger is per project, not per session: the ruling is about the files,
//! so a later session does not see the same untouched files again either.
//!
//! Every answer that is not a positive "all of these were shown" — a file
//! that cannot be read, a ledger that cannot be read, no files named — keeps
//! the gate's normal block (CLAUDE.md §3: "cannot tell" is not "already
//! warned").

use std::path::{Path, PathBuf};

use harness_core::hash::fnv1a64;
use harness_core::verdict::Determination;

/// `<path>\t<hash>` for each of `files` (repo-relative) as it is under `root`.
///
/// A file that does not exist hashes as `absent` (a deletion is a real state);
/// any other read error is `Undetermined`.
pub fn entries(root: &Path, files: &[String]) -> Determination<Vec<String>> {
    if files.is_empty() {
        return Determination::undetermined("no implementation files named");
    }
    let mut out = Vec::new();
    for f in files {
        let hash = match std::fs::read(root.join(f)) {
            Ok(bytes) => format!("{:016x}", fnv1a64(&bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => "absent".to_string(),
            Err(e) => return Determination::undetermined(format!("cannot read {f}: {e}")),
        };
        let entry = format!("{f}\t{hash}");
        if !out.contains(&entry) {
            out.push(entry);
        }
    }
    Determination::known(out)
}

fn ledger(state_dir: &Path, root: &Path) -> PathBuf {
    state_dir
        .join("warned")
        .join(harness_core::projkey::project_key(root))
}

/// Whether EVERY one of `entries` was already shown for this project.
pub fn all_seen(state_dir: &Path, root: &Path, entries: &[String]) -> Determination<bool> {
    if entries.is_empty() {
        return Determination::undetermined("no entries to look up");
    }
    match std::fs::read_to_string(ledger(state_dir, root)) {
        Ok(text) => {
            let shown: Vec<&str> = text.lines().map(str::trim_end).collect();
            Determination::known(entries.iter().all(|e| shown.contains(&e.as_str())))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Determination::known(false),
        Err(e) => Determination::undetermined(format!("cannot read warned ledger: {e}")),
    }
}

/// Remember that each of `entries` has now been shown for this project.
pub fn record(state_dir: &Path, root: &Path, entries: &[String]) -> std::io::Result<()> {
    let path = ledger(state_dir, root);
    for e in entries {
        harness_core::append::append_line(&path, e)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tdd-warned-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn known<T: std::fmt::Debug>(d: Determination<T>) -> T {
        match d {
            Determination::Known(v) => v,
            Determination::Undetermined(r) => panic!("expected Known, got {r:?}"),
        }
    }

    #[test]
    fn per_file_hash_is_shown_once_and_an_edit_shows_it_again() {
        let root = tmp("root");
        let state = tmp("state");
        std::fs::write(root.join("a.js"), "one").unwrap();
        std::fs::write(root.join("b.js"), "one").unwrap();
        let a = known(entries(&root, &["a.js".to_string()]));
        assert!(!known(all_seen(&state, &root, &a)));
        record(&state, &root, &a).unwrap();
        assert!(known(all_seen(&state, &root, &a)));

        // a.js unchanged + a new file b.js: b.js is unseen, so it is shown.
        let ab = known(entries(&root, &["a.js".to_string(), "b.js".to_string()]));
        assert!(!known(all_seen(&state, &root, &ab)));

        // a.js edited: new hash, shown again.
        std::fs::write(root.join("a.js"), "two").unwrap();
        let a2 = known(entries(&root, &["a.js".to_string()]));
        assert!(!known(all_seen(&state, &root, &a2)));
    }
}
