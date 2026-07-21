//! Determine which files have changed so checks can be scoped (don't run
//! `cargo test` when only markdown changed). Pure subprocess calls to `git`.

use harness_core::git_probe::{probe_repo, RepoProbe};
use std::path::Path;
use std::process::Command;

/// Changed paths relative to the repo root: tracked changes vs HEAD plus
/// untracked-but-not-ignored files. `None` means "no usable changed-file set" —
/// out of scope, or the repository state could not be determined. Callers then
/// treat every check as applicable, which is the restrictive reading, so unlike
/// the sibling gates both cases can share this branch. Repository detection
/// itself goes through `harness_core::git_probe`.
pub fn changed_files(root: &Path) -> Option<Vec<String>> {
    // `None` here is donegate's RESTRICTIVE branch (the caller then treats every
    // check as applicable), so both non-`Repo` answers collapse into it safely —
    // unlike the sibling gates, where `NotRepo` means allow. The shared probe is
    // still used so this copy cannot drift back into its own `bool` version.
    if probe_repo(root) != RepoProbe::Repo {
        return None;
    }
    let mut out = Vec::new();
    // tracked, unstaged changes
    collect(root, &["diff", "--name-only"], &mut out);
    // staged changes — also the only signal in a repo with no commits yet, where
    // `diff HEAD` errors out (no HEAD to diff against).
    collect(root, &["diff", "--cached", "--name-only"], &mut out);
    // untracked (respecting .gitignore)
    collect(
        root,
        &["ls-files", "--others", "--exclude-standard"],
        &mut out,
    );
    out.sort();
    out.dedup();
    Some(out)
}

fn collect(root: &Path, args: &[&str], out: &mut Vec<String>) {
    if let Ok(o) = Command::new("git").current_dir(root).args(args).output() {
        if o.status.success() {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                let line = line.trim();
                if !line.is_empty() {
                    out.push(line.to_string());
                }
            }
        }
    }
}

/// RED tests for backlog 7d3db473 (donegate's share).
///
/// donegate's `is_git_repo` is the same "git could not be run ⇒ not a repo"
/// collapse as tdd/reviewgate/propguard, but its CONSUMER is restrictive: a
/// `None` from `changed_files` makes every check applicable (`gate::applies`
/// returns true for `&None`). So donegate is not fail-open at the consumer and
/// these tests deliberately assert NO block — only that the shared probe
/// reports `Undetermined` here, and that donegate keeps resolving that to the
/// restrictive `None` rather than to a `Some(vec![])` "nothing changed" that
/// would silently narrow every scoped check out of existence.
#[cfg(test)]
mod tests {
    use super::*;
    use harness_core::git_probe::{probe_repo, RepoProbe};
    use std::path::PathBuf;
    use std::sync::Mutex;

    /// Serializes the tests that mutate the process-global `PATH`. donegate's
    /// other unit tests (config/gate/install) do not spawn subprocesses, and
    /// this module's own git-spawning code is only reached from inside these
    /// serialized tests, so the mutation window is covered.
    static PROBE_PATH_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn scratch_dir(tag: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "donegate-probe-{}-{}-{}",
            tag,
            std::process::id(),
            n
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("create scratch dir");
        p
    }

    /// Independent of the code under test.
    fn has_dot_git_ancestor(dir: &Path) -> bool {
        let mut cur = Some(dir);
        while let Some(d) = cur {
            if std::fs::symlink_metadata(d.join(".git")).is_ok() {
                return true;
            }
            cur = d.parent();
        }
        false
    }

    /// Apparatus check: with this PATH, git genuinely cannot be spawned — so an
    /// `Undetermined` below cannot be passing for the wrong reason.
    #[test]
    fn empty_path_really_has_no_git() {
        let _g = PROBE_PATH_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root = scratch_dir("apparatus");
        let empty_bin = root.join("empty-bin");
        std::fs::create_dir_all(&empty_bin).unwrap();
        assert!(!empty_bin.join("git").exists(), "PATH dir must have no git");

        let old = std::env::var_os("PATH");
        std::env::set_var("PATH", &empty_bin);
        let spawned = Command::new("git").arg("--version").output();
        match old {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }

        assert!(
            spawned.is_err(),
            "git must be unreachable under the emptied PATH; it was spawnable, so the probe \
             test below would not exercise the spawn-failure path"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The probe result itself: unspawnable git over a directory that HAS a
    /// `.git` is UNDETERMINED, not NotRepo.
    #[test]
    fn unspawnable_git_over_a_repo_is_undetermined() {
        let _g = PROBE_PATH_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root = scratch_dir("unspawnable");
        let empty_bin = root.join("empty-bin");
        std::fs::create_dir_all(&empty_bin).unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(work.join(".git")).unwrap();

        let old = std::env::var_os("PATH");
        std::env::set_var("PATH", &empty_bin);
        let probe = probe_repo(&work);
        let changed = changed_files(&work);
        match old {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }

        assert_eq!(
            probe,
            RepoProbe::Undetermined,
            "a .git exists but git could not be run: the answer is unknown. Reporting NotRepo \
             here is the shared fail-open (harmless in donegate, an ALLOW in tdd/reviewgate/\
             propguard)"
        );
        assert!(
            changed.is_none(),
            "donegate must resolve an undetermined probe to `None` — the RESTRICTIVE branch, \
             where every check stays applicable. A `Some(vec![])` here would read as 'nothing \
             changed' and silently skip every `when_changed` check"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Over-block guard: with no git AND no `.git` anywhere, the directory is
    /// genuinely out of scope — the probe must say NotRepo (donegate's `None`
    /// is unchanged either way, but the probe's answer must not be inflated).
    #[test]
    fn unspawnable_git_without_a_dot_git_is_not_repo() {
        let _g = PROBE_PATH_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root = scratch_dir("bare");
        let empty_bin = root.join("empty-bin");
        std::fs::create_dir_all(&empty_bin).unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        if has_dot_git_ancestor(&work) {
            eprintln!(
                "SKIPPED unspawnable_git_without_a_dot_git_is_not_repo: {} has a .git ancestor \
                 on this machine, so the 'no .git anywhere' case cannot be constructed here",
                work.display()
            );
            let _ = std::fs::remove_dir_all(&root);
            return;
        }

        let old_path = std::env::var_os("PATH");
        let old_gitdir = std::env::var_os("GIT_DIR");
        std::env::remove_var("GIT_DIR");
        std::env::set_var("PATH", &empty_bin);
        let probe = probe_repo(&work);
        match old_path {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
        if let Some(v) = old_gitdir {
            std::env::set_var("GIT_DIR", v);
        }

        assert_eq!(
            probe,
            RepoProbe::NotRepo,
            "nothing corroborates a repo here; an Undetermined would be an over-block for the \
             gates that consume the same probe"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
