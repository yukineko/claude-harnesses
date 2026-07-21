//! Determine which files have changed so checks can be scoped (don't run
//! `cargo test` when only markdown changed). Pure subprocess calls to `git`.

use harness_core::git_probe::{probe_repo, RepoProbe};
use std::path::Path;
use std::process::Command;

/// Tri-state result of scanning the working tree, mirroring the sibling gates
/// (`reviewgate`/`tdd`/`propguard`) so donegate can no longer drift into its own
/// shape. `NotRepo` = confirmed out of scope; `Failed` = the repo state could not
/// be determined (git could not be run, or a sub-command errored); `Files` = a
/// successful scan (possibly empty for a clean tree).
///
/// donegate's CONSUMER is restrictive: it maps BOTH `NotRepo` and `Failed` to
/// "every check applies" (unlike the siblings, where `NotRepo` means allow). The
/// point of the tri-state here is that `Failed` must NOT collapse into a
/// successful empty scan: a git sub-command that errored used to leave an empty
/// file set, which read as "nothing changed" and silently skipped every
/// `when_changed` check — passing the Stop gate on an undetermined tree. `Failed`
/// now keeps every check applicable instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeScan {
    NotRepo,
    Failed,
    Files(Vec<String>),
}

/// Changed paths relative to the repo root: tracked changes vs HEAD, staged
/// changes, and untracked-but-not-ignored files. Returns `NotRepo` when there is
/// no git repo, `Failed` when the repo state is undetermined or any sub-command
/// errored (non-zero exit / spawn failure), and `Files` on a successful scan.
/// Repository detection goes through `harness_core::git_probe`.
pub fn changed_files(root: &Path) -> ChangeScan {
    match probe_repo(root) {
        RepoProbe::Repo => {}
        // Genuinely out of scope. donegate maps this to the restrictive branch in
        // its consumer, but the scan result itself must stay honest (`NotRepo`, not
        // an empty `Files`) so it mirrors the sibling gates' shared shape.
        RepoProbe::NotRepo => return ChangeScan::NotRepo,
        // git could not be run / refused to answer while a `.git` exists: unknown,
        // not "no scope" → fail closed.
        RepoProbe::Undetermined => return ChangeScan::Failed,
    }
    let mut out = Vec::new();
    // If ANY sub-command errors, the changed set is undetermined → fail closed.
    // A clean repo's commands all exit 0 with empty stdout → Files(vec![]).
    let ok = collect(root, &["diff", "--name-only"], &mut out)
        // staged changes — also the only signal in a repo with no commits yet,
        // where `diff HEAD` errors out (no HEAD to diff against).
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

/// Run one `git` sub-command, appending its trimmed non-empty stdout lines to
/// `out`. Returns `true` on success (exit 0), `false` on a spawn error or a
/// non-zero exit — the caller maps `false` to `ChangeScan::Failed`. A successful
/// command with EMPTY stdout still returns `true` (clean ≠ failed).
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

/// RED tests for backlog 7d3db473 (donegate's share).
///
/// donegate's `is_git_repo` is the same "git could not be run ⇒ not a repo"
/// collapse as tdd/reviewgate/propguard, but its CONSUMER is restrictive: a
/// `ChangeScan::NotRepo`/`Failed` from `changed_files` is mapped to `None` in
/// `gate::evaluate`, which makes every check applicable (`gate::applies` returns
/// true for `&None`). So donegate is not fail-open at the consumer and these
/// tests deliberately assert NO block — only that the shared probe reports
/// `Undetermined` here, and that donegate keeps resolving that to `Failed`
/// (→ restrictive) rather than to a `Files(vec![])` "nothing changed" that would
/// silently narrow every scoped check out of existence.
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
        assert_eq!(
            changed,
            ChangeScan::Failed,
            "donegate must resolve an undetermined probe to `Failed` — its consumer maps that to \
             the RESTRICTIVE branch where every check stays applicable. A `Files(vec![])` here \
             would read as 'nothing changed' and silently skip every `when_changed` check"
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

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// A directory that is not a git repo → `NotRepo` (donegate maps that to the
    /// restrictive branch, but the scan result itself must be honest, not Files).
    #[test]
    fn non_repo_dir_is_notrepo() {
        let root = scratch_dir("notrepo");
        if has_dot_git_ancestor(&root) {
            eprintln!("SKIPPED non_repo_dir_is_notrepo: scratch has a .git ancestor here");
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        assert_eq!(changed_files(&root), ChangeScan::NotRepo);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A clean repo is a SUCCESSFUL empty scan → `Files(vec![])`, never `Failed`.
    /// Pins that a clean tree (git diff exits 0 with empty stdout) does NOT start
    /// forcing every check under the fail-closed change (no over-block).
    #[test]
    fn clean_repo_is_empty_files_not_failed() {
        if !git_available() {
            eprintln!("skipping clean_repo_is_empty_files_not_failed: git not available");
            return;
        }
        let root = scratch_dir("clean");
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
        for args in [&["add", "a.txt"][..], &["commit", "-qm", "init"][..]] {
            assert!(Command::new("git")
                .current_dir(root)
                .args(args)
                .status()
                .unwrap()
                .success());
        }
        assert_eq!(
            changed_files(root),
            ChangeScan::Files(Vec::new()),
            "a clean repo must be a successful empty scan, not Failed"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// RED→GREEN for 1ddd37e9: `collect` must distinguish an errored sub-command
    /// from an empty-but-successful one. A bogus git flag exits non-zero → `false`
    /// → `changed_files` returns `Failed` (the old code swallowed it into an empty
    /// `Some(vec![])`, silently skipping every `when_changed` check). A valid
    /// command with no output → `true` (clean ≠ failed).
    #[test]
    fn collect_reports_error_vs_empty_success() {
        if !git_available() {
            eprintln!("skipping collect_reports_error_vs_empty_success: git not available");
            return;
        }
        let root = scratch_dir("collect");
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
        // Bogus flag → non-zero exit → false, so the scan fails closed to Failed.
        assert!(
            !collect(root, &["diff", "--no-such-flag-xyzzy"], &mut out),
            "a non-zero git exit must report false so the scan fails closed"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
