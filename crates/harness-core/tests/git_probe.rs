//! RED tests for the shared git-repo probe (`harness_core::git_probe`) —
//! backlog 7d3db473.
//!
//! The defect these pin: four gate crates each carry a private `is_git_repo`
//! that collapses "git could not be run / refused to answer" into "this is not
//! a git repo". Three of their consumers map that `false` straight to an ALLOW
//! (`reviewgate::review` `NotRepo => allow("no-git")`, `propguard::gate` ditto,
//! `tdd::gate` `NotRepo => git_unscoped => allow`). So in a PATH-restricted
//! hook, under fork failure/EMFILE, or in a repo git refuses to answer about
//! (dubious ownership, corruption), all three gates silently allow every stop.
//!
//! The fix is a single tri-state probe: a git failure may only be read as "out
//! of scope" when INDEPENDENT filesystem evidence (a `.git` entry, or a GIT_DIR
//! override) corroborates it. Otherwise the answer is `Undetermined`, which
//! consumers must resolve to the restrictive side.
//!
//! These tests live in `tests/` (their own test binary) on purpose: the
//! `probe_repo` wiring tests mutate the process-global `PATH`, and a separate
//! process keeps that mutation away from harness-core's in-crate unit tests,
//! which spawn git without any such lock.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use harness_core::git_probe::{decide_repo_probe, dot_git_present, probe_repo, RepoProbe};

/// Serializes every test in this file that mutates the process-global `PATH` /
/// `GIT_DIR`, or that spawns git and would therefore be corrupted by another
/// test's PATH mutation. Mirrors `condukt::oracle::tests::ORACLE_PATH_ENV_LOCK`.
///
/// Scope caveat (deliberately not hidden): this lock only covers the tests in
/// THIS test binary. Other harness-core tests that spawn git run in different
/// processes (each `tests/*.rs` is its own binary, and unit tests live in the
/// lib test binary), so they cannot see this PATH mutation — which is exactly
/// why these tests are here rather than inline in `src/git_probe.rs`.
static PROBE_PATH_ENV_LOCK: Mutex<()> = Mutex::new(());

// ─── decision core: every row of the decision table ────────────────────────
//
// Column order is (spawned_ok, exit_ok, stdout, dot_git_found).

/// Row 1: git ran, exited 0, said "true" → we are inside a work tree.
#[test]
fn decides_repo_when_git_says_true() {
    for dot_git in [true, false] {
        assert_eq!(
            decide_repo_probe(true, true, "true", dot_git),
            RepoProbe::Repo,
            "git answered authoritatively that this IS a work tree (dot_git_found={dot_git}); \
             anything but Repo would take a real repo out of every gate's scope"
        );
    }
}

/// git's real stdout carries a trailing newline. If the implementation compares
/// raw stdout it will read `"true\n"` as unparseable and return `Undetermined`
/// for EVERY real invocation — the gates would block constantly and get
/// disabled by the operator, which is a fail-open by another road.
#[test]
fn decides_repo_when_git_says_true_with_trailing_newline() {
    assert_eq!(
        decide_repo_probe(true, true, "true\n", false),
        RepoProbe::Repo,
        "git prints `true\\n`; the decision core must trim before comparing, or every real \
         probe is Undetermined"
    );
}

/// Row 2: git ran, exited 0, said "false" — the authoritative answer for a dir
/// inside a `.git` directory. Genuinely out of scope.
#[test]
fn decides_not_repo_when_git_says_false() {
    for dot_git in [true, false] {
        assert_eq!(
            decide_repo_probe(true, true, "false", dot_git),
            RepoProbe::NotRepo,
            "git authoritatively answered `false` (dot_git_found={dot_git}); this must stay \
             NotRepo → allow, or the fix over-blocks in a genuinely out-of-scope directory"
        );
    }
    assert_eq!(
        decide_repo_probe(true, true, "false\n", true),
        RepoProbe::NotRepo,
        "trailing newline must not change an authoritative `false`"
    );
}

/// Row 3: git exited 0 but printed something we cannot read (empty output, a
/// truncated pipe, a locale-translated or future answer). Success without a
/// readable verdict is NOT evidence of anything.
#[test]
fn decides_undetermined_when_git_succeeds_with_unreadable_stdout() {
    for stdout in [
        "",
        "\n",
        "   ",
        "yes",
        "TRUE!",
        "fatal: whatever",
        "true false",
    ] {
        for dot_git in [true, false] {
            assert_eq!(
                decide_repo_probe(true, true, stdout, dot_git),
                RepoProbe::Undetermined,
                "git exited 0 but printed {stdout:?} (dot_git_found={dot_git}); reading that as \
                 NotRepo would take the whole repo out of scope and allow every stop unchecked"
            );
        }
    }
}

/// Row 4: git ran and FAILED, but a `.git` exists. "Not a repo" is then not a
/// credible reading — this is the dubious-ownership / corrupt-repo shape.
#[test]
fn decides_undetermined_when_git_fails_but_dot_git_exists() {
    for stdout in [
        "",
        "fatal: detected dubious ownership in repository",
        "true",
    ] {
        assert_eq!(
            decide_repo_probe(true, false, stdout, true),
            RepoProbe::Undetermined,
            "git errored ({stdout:?}) inside a directory that HAS a .git; collapsing that to \
             NotRepo is the exact fail-open this closes — every gate would allow the stop in a \
             repo git merely refused to answer about"
        );
    }
}

/// Row 5: git ran and failed, and there is no `.git` anywhere. Independent
/// evidence corroborates "out of scope" — stay NotRepo (preserves the existing
/// correct behaviour; git exits non-zero outside a work tree).
#[test]
fn decides_not_repo_when_git_fails_and_no_dot_git() {
    assert_eq!(
        decide_repo_probe(true, false, "", false),
        RepoProbe::NotRepo,
        "git's non-zero exit outside a work tree, corroborated by no .git on disk, is the \
         ordinary non-repo case; making it Undetermined would block every stop in every \
         non-repo directory"
    );
}

/// Row 6: git could not be spawned at all (PATH-restricted hook, fork failure,
/// EMFILE) but a `.git` exists → Undetermined.
#[test]
fn decides_undetermined_when_spawn_fails_but_dot_git_exists() {
    assert_eq!(
        decide_repo_probe(false, false, "", true),
        RepoProbe::Undetermined,
        "git never ran, yet a .git is on disk: this IS a repo we simply could not inspect. \
         NotRepo here is the PATH-restricted-hook fail-open (all three gates allow)"
    );
    // exit_ok is meaningless when nothing was spawned; a `true` there must not
    // be able to manufacture a Repo/NotRepo answer.
    assert_eq!(
        decide_repo_probe(false, true, "true", true),
        RepoProbe::Undetermined,
        "with spawned_ok=false there is no exit status and no stdout to trust, whatever the \
         other arguments say"
    );
}

/// Row 7: git could not be spawned and there is no `.git` anywhere → NotRepo.
#[test]
fn decides_not_repo_when_spawn_fails_and_no_dot_git() {
    assert_eq!(
        decide_repo_probe(false, false, "", false),
        RepoProbe::NotRepo,
        "no git and no .git: nothing suggests a repo, so the gates legitimately have no scope"
    );
    assert_eq!(
        decide_repo_probe(false, true, "true", false),
        RepoProbe::NotRepo,
        "spawned_ok=false must ignore the stale exit_ok/stdout arguments here too"
    );
}

// ─── dot_git_present: filesystem corroboration ─────────────────────────────

fn scratch_dir(tag: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!(
        "harness-core-git-probe-{}-{}-{}",
        tag,
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("create scratch dir");
    p
}

/// Independent (does not use the function under test) check for a `.git` entry
/// at or above `dir`. Used as a PRECONDITION oracle for the negative test.
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

#[test]
fn dot_git_directory_is_present() {
    let root = scratch_dir("dir");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    assert!(
        dot_git_present(&root),
        "an ordinary repo's `.git` DIRECTORY is the primary corroboration; missing it makes \
         every unspawnable-git probe read as NotRepo → allow"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn dot_git_file_worktree_form_is_present() {
    let root = scratch_dir("file");
    std::fs::write(root.join(".git"), "gitdir: /somewhere/.git/worktrees/wt\n").unwrap();
    assert!(
        dot_git_present(&root),
        "in a git WORKTREE `.git` is a FILE, not a directory. A dir-only check would leave \
         every worktree (this repo's own layout) uncorroborated → NotRepo → allow"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn dot_git_is_found_from_a_nested_subdirectory() {
    for (tag, make_file) in [("nested-dir", false), ("nested-file", true)] {
        let root = scratch_dir(tag);
        if make_file {
            std::fs::write(root.join(".git"), "gitdir: /somewhere\n").unwrap();
        } else {
            std::fs::create_dir_all(root.join(".git")).unwrap();
        }
        let deep = root.join("a").join("b").join("c");
        std::fs::create_dir_all(&deep).unwrap();
        assert!(
            dot_git_present(&deep),
            "the probe runs from the hook's cwd, which is usually DEEP inside the repo; if the \
             walk up to the repo root is missing, every such probe is uncorroborated → NotRepo \
             → allow ({tag})"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// The negative side: no `.git` anywhere up to the filesystem root → false.
///
/// Determinism caveat (stated rather than hidden): a temp dir COULD sit under a
/// real repo on some machine, which would make the expected answer genuinely
/// `true`. The precondition is therefore checked with `has_dot_git_ancestor`,
/// which is independent of the function under test; if it does not hold, the
/// test reports that the environment cannot express this case instead of
/// asserting something that is only true on this machine.
#[test]
fn no_dot_git_anywhere_is_absent() {
    let _guard = PROBE_PATH_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let root = scratch_dir("absent");
    if has_dot_git_ancestor(&root) {
        eprintln!(
            "SKIPPED no_dot_git_anywhere_is_absent: {} has a .git ancestor on this machine, so \
             the 'no .git anywhere' case cannot be constructed here",
            root.display()
        );
        let _ = std::fs::remove_dir_all(&root);
        return;
    }
    // GIT_DIR must not be set, or the probe is entitled to answer true.
    let had_git_dir = std::env::var_os("GIT_DIR");
    std::env::remove_var("GIT_DIR");
    let got = dot_git_present(&root);
    if let Some(v) = had_git_dir {
        std::env::set_var("GIT_DIR", v);
    }
    assert!(
        !got,
        "a directory with no .git at or above it must NOT be corroborated as a repo, or the \
         probe would return Undetermined (block) for every ordinary non-repo directory"
    );
    let _ = std::fs::remove_dir_all(&root);
}

// ─── probe_repo: the wired spawn path ──────────────────────────────────────

/// Prepare a PATH that contains exactly one empty directory, so `git` cannot be
/// found and `Command::new("git").output()` fails to spawn. Returns the old
/// PATH for restoration.
fn strip_git_from_path(empty_bin: &Path) -> Option<std::ffi::OsString> {
    let old = std::env::var_os("PATH");
    std::env::set_var("PATH", empty_bin);
    old
}

fn restore_path(old: Option<std::ffi::OsString>) {
    match old {
        Some(p) => std::env::set_var("PATH", p),
        None => std::env::remove_var("PATH"),
    }
}

/// Apparatus check: the emptied PATH really does make `git` unspawnable. Without
/// this, the Undetermined assertions below could pass for the wrong reason (e.g.
/// git ran fine and answered something unreadable).
#[test]
fn empty_path_really_has_no_git() {
    let _guard = PROBE_PATH_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let root = scratch_dir("apparatus");
    let empty_bin = root.join("empty-bin");
    std::fs::create_dir_all(&empty_bin).unwrap();
    assert!(
        !empty_bin.join("git").exists(),
        "the fake PATH dir must contain no git"
    );

    let old = strip_git_from_path(&empty_bin);
    let spawned = Command::new("git").arg("--version").output();
    restore_path(old);

    assert!(
        spawned.is_err(),
        "with PATH={} the git binary must be unreachable; it was spawnable, so the probe tests \
         below would not be exercising the spawn-failure path at all",
        empty_bin.display()
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// THE defect, wired: git unreachable over a directory that DOES contain a
/// `.git` must be `Undetermined` — never `NotRepo` (which every gate reads as
/// "no scope → allow").
#[test]
fn probe_repo_is_undetermined_when_git_unspawnable_over_a_dot_git_dir() {
    let _guard = PROBE_PATH_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let root = scratch_dir("unspawnable");
    let empty_bin = root.join("empty-bin");
    std::fs::create_dir_all(&empty_bin).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(work.join(".git")).unwrap();

    let old = strip_git_from_path(&empty_bin);
    let got = probe_repo(&work);
    restore_path(old);

    assert_eq!(
        got,
        RepoProbe::Undetermined,
        "a PATH-restricted hook (no git) over a real repo must be UNDETERMINED. NotRepo here is \
         the production fail-open: reviewgate/propguard/tdd all allow the stop unreviewed"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Same, for the git-worktree `.git` FILE form (this repo's own layout).
#[test]
fn probe_repo_is_undetermined_when_git_unspawnable_over_a_dot_git_file() {
    let _guard = PROBE_PATH_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let root = scratch_dir("unspawnable-wt");
    let empty_bin = root.join("empty-bin");
    std::fs::create_dir_all(&empty_bin).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join(".git"), "gitdir: /somewhere/.git/worktrees/wt\n").unwrap();

    let old = strip_git_from_path(&empty_bin);
    let got = probe_repo(&work);
    restore_path(old);

    assert_eq!(
        got,
        RepoProbe::Undetermined,
        "worktree checkouts carry a .git FILE; if only directories corroborate, every worktree \
         run of a PATH-restricted hook silently allows"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A `GIT_DIR` override is also independent evidence that we are operating on a
/// repo, even with no `.git` next to the cwd.
#[test]
fn probe_repo_is_undetermined_when_git_unspawnable_under_git_dir_override() {
    let _guard = PROBE_PATH_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let root = scratch_dir("gitdir-env");
    let empty_bin = root.join("empty-bin");
    std::fs::create_dir_all(&empty_bin).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let gitdir = root.join("elsewhere.git");
    std::fs::create_dir_all(&gitdir).unwrap();
    if has_dot_git_ancestor(&work) {
        eprintln!("SKIPPED probe_repo_..._git_dir_override: scratch dir has a .git ancestor");
        let _ = std::fs::remove_dir_all(&root);
        return;
    }

    let old_path = strip_git_from_path(&empty_bin);
    let old_gitdir = std::env::var_os("GIT_DIR");
    std::env::set_var("GIT_DIR", &gitdir);
    let got = probe_repo(&work);
    match old_gitdir {
        Some(v) => std::env::set_var("GIT_DIR", v),
        None => std::env::remove_var("GIT_DIR"),
    }
    restore_path(old_path);

    assert_eq!(
        got,
        RepoProbe::Undetermined,
        "GIT_DIR names a repo explicitly; with git unreachable the answer is unknown, not \
         'out of scope'"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Preserve existing correct behaviour (no over-block): git unreachable over a
/// directory with no `.git` anywhere is still `NotRepo`.
#[test]
fn probe_repo_is_not_repo_when_git_unspawnable_and_no_dot_git() {
    let _guard = PROBE_PATH_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let root = scratch_dir("unspawnable-bare");
    let empty_bin = root.join("empty-bin");
    std::fs::create_dir_all(&empty_bin).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    if has_dot_git_ancestor(&work) {
        eprintln!(
            "SKIPPED probe_repo_is_not_repo_when_git_unspawnable_and_no_dot_git: scratch dir \
             has a .git ancestor on this machine"
        );
        let _ = std::fs::remove_dir_all(&root);
        return;
    }
    let old_gitdir = std::env::var_os("GIT_DIR");
    std::env::remove_var("GIT_DIR");

    let old = strip_git_from_path(&empty_bin);
    let got = probe_repo(&work);
    restore_path(old);
    if let Some(v) = old_gitdir {
        std::env::set_var("GIT_DIR", v);
    }

    assert_eq!(
        got,
        RepoProbe::NotRepo,
        "nothing suggests a repo here; answering Undetermined would block every stop taken in \
         an ordinary non-repo directory — an over-block the fix must not introduce"
    );
    let _ = std::fs::remove_dir_all(&root);
}

// ─── preserved behaviour with a REAL git on PATH ───────────────────────────

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A real repo must still be `Repo`.
#[test]
fn probe_repo_reports_repo_for_a_real_repository() {
    let _guard = PROBE_PATH_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if !git_available() {
        eprintln!("SKIPPED probe_repo_reports_repo_for_a_real_repository: git not available");
        return;
    }
    let root = scratch_dir("real-repo");
    assert!(Command::new("git")
        .current_dir(&root)
        .args(["init", "-q"])
        .status()
        .expect("git init")
        .success());
    assert_eq!(
        probe_repo(&root),
        RepoProbe::Repo,
        "a working git repo must be Repo, or the gates lose their normal scope entirely"
    );
    let deep = root.join("x").join("y");
    std::fs::create_dir_all(&deep).unwrap();
    assert_eq!(
        probe_repo(&deep),
        RepoProbe::Repo,
        "a subdirectory of a repo is inside the work tree too"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A genuine non-repo directory must still be `NotRepo` → the consumers' allow
/// path is unchanged. This is the over-block guard for the real-git path.
#[test]
fn probe_repo_reports_not_repo_for_a_genuine_non_repo_dir() {
    let _guard = PROBE_PATH_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if !git_available() {
        eprintln!("SKIPPED probe_repo_reports_not_repo_for_a_genuine_non_repo_dir: no git");
        return;
    }
    let root = scratch_dir("real-nonrepo");
    if has_dot_git_ancestor(&root) {
        eprintln!("SKIPPED probe_repo_reports_not_repo_...: scratch dir has a .git ancestor");
        let _ = std::fs::remove_dir_all(&root);
        return;
    }
    let old_gitdir = std::env::var_os("GIT_DIR");
    std::env::remove_var("GIT_DIR");
    let got = probe_repo(&root);
    if let Some(v) = old_gitdir {
        std::env::set_var("GIT_DIR", v);
    }
    assert_eq!(
        got,
        RepoProbe::NotRepo,
        "an ordinary directory outside any repo must stay NotRepo (→ allow); otherwise the fix \
         turns every non-repo session into a permanent block"
    );
    let _ = std::fs::remove_dir_all(&root);
}
