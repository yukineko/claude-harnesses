//! Deterministic fingerprint of the SKILL.md corpus under a directory.
//!
//! A silent edit to a `SKILL.md` shifts agent behaviour without changing any
//! recorded field, so outcomes drift away from the skill version that produced
//! them. Stamping each `Episode` with this fingerprint lets us stratify outcomes
//! by skill version after the fact. std-only (DefaultHasher), mirroring the id
//! hashing tracekit uses — no external crate just for this.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

/// Walk `root` recursively for files named exactly `SKILL.md`, hash their
/// sorted (relative-path, content) pairs, and return a short lowercase hex
/// string. Deterministic: the same corpus always yields the same hex, and any
/// changed/added/removed SKILL.md changes it. Unreadable files are skipped
/// rather than aborting the whole walk (fail-soft, like the store), but the
/// skip is not silent — see `collect_skills`.
pub fn skill_fingerprint(root: &Path) -> std::io::Result<String> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    collect_skills(root, root, &mut pairs);
    // Sort by relative path so directory-iteration order can't perturb the hash.
    pairs.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher = DefaultHasher::new();
    for (rel, content) in &pairs {
        // Hash path and content as distinct, length-delimited fields (Hash for
        // str already feeds a length) so "ab"+"c" can't collide with "a"+"bc".
        rel.hash(&mut hasher);
        content.hash(&mut hasher);
    }
    let hash = hasher.finish();
    Ok(format!("{hash:016x}"))
}

/// Recursively gather (relative-path-from-`base`, content) for every `SKILL.md`.
/// A single bad path (unreadable dir/file, non-UTF8 content) is skipped so it
/// never sinks the whole fingerprint walk — BUT unlike a merely-vanished path,
/// it is not silent: a `SKILL.md` that exists but can't be read defeats this
/// module's entire purpose (detecting a silent skill edit), so every
/// non-NotFound error is surfaced via `eprintln!` while the walk continues.
fn collect_skills(base: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            eprintln!(
                "warning: collect_skills: cannot read {}: {e}",
                dir.display()
            );
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                eprintln!(
                    "warning: collect_skills: unreadable entry in {}: {e}",
                    dir.display()
                );
                continue;
            }
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(e) => {
                eprintln!(
                    "warning: collect_skills: cannot stat {}: {e}",
                    path.display()
                );
                continue;
            }
        };
        if file_type.is_dir() {
            collect_skills(base, &path, out);
        } else if file_type.is_file() && path.file_name() == Some(std::ffi::OsStr::new("SKILL.md"))
        {
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!(
                        "warning: collect_skills: cannot read {}: {e} (fingerprint will not reflect this file)",
                        path.display()
                    );
                    continue;
                }
            };
            let rel = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            out.push((rel, content));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A throwaway temp dir under the system temp, keyed by pid + tag so parallel
    /// tests don't collide. Returns the created path.
    fn tmp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fugu-fingerprint-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn stable_across_calls_same_tree() {
        let dir = tmp_dir("stable");
        write(&dir.join("a/SKILL.md"), "alpha skill");
        write(&dir.join("b/SKILL.md"), "beta skill");
        // a non-SKILL file must not affect the fingerprint
        write(&dir.join("a/README.md"), "ignored");

        let first = skill_fingerprint(&dir).unwrap();
        let second = skill_fingerprint(&dir).unwrap();
        assert_eq!(first, second, "same corpus must hash identically");
        assert!(!first.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn changes_when_skill_content_changes() {
        let dir = tmp_dir("changed");
        let skill = dir.join("a/SKILL.md");
        write(&skill, "original guidance");
        let before = skill_fingerprint(&dir).unwrap();

        write(&skill, "edited guidance");
        let after = skill_fingerprint(&dir).unwrap();
        assert_ne!(
            before, after,
            "an edited SKILL.md must change the fingerprint"
        );

        // adding another SKILL.md must also change it
        write(&dir.join("c/SKILL.md"), "new skill");
        let with_added = skill_fingerprint(&dir).unwrap();
        assert_ne!(
            after, with_added,
            "an added SKILL.md must change the fingerprint"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_or_missing_tree_is_stable() {
        let dir = tmp_dir("empty");
        let a = skill_fingerprint(&dir).unwrap();
        let b = skill_fingerprint(&dir).unwrap();
        assert_eq!(a, b);
        // a missing root reads as an empty corpus rather than erroring
        let missing = dir.join("does-not-exist");
        assert_eq!(skill_fingerprint(&missing).unwrap(), a);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_subdir_does_not_panic_and_still_hashes_readable_skills() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp_dir("unreadable-subdir");
        write(&dir.join("visible/SKILL.md"), "visible skill");
        let locked = dir.join("locked");
        write(&locked.join("SKILL.md"), "hidden skill");
        let mut perms = std::fs::metadata(&locked).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&locked, perms.clone()).unwrap();

        let result = std::panic::catch_unwind(|| skill_fingerprint(&dir));

        perms.set_mode(0o755);
        std::fs::set_permissions(&locked, perms).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert!(result.is_ok(), "must not panic on an unreadable subdir");
        let fp = result.unwrap().unwrap();
        assert!(!fp.is_empty(), "must still hash the readable skill");
    }
}
