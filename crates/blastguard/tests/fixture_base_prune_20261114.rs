//! Regression tests for the two deletions `support/neutral_base.rs` performs
//! on fixture run directories (backlog 37955e32; coordinator rulings R2/R3):
//! the exit-time SELF-CLEAN of this process's own `run-*` dir, and the PRUNE of
//! stale `run-*` dirs directly under the dedicated
//! `blastguard-worktree-root-fixtures` directory.
//!
//! The functions under test are the production ones (`prune_stale_runs`,
//! `clean_own_run`; the latter is the body `self_clean` calls). They take the
//! directory as a parameter, so every case runs in a fake layout
//! `<scratch>/<case>/blastguard-worktree-root-fixtures/` created
//! with per-test unique names under the system
//! temp dir. The real dedicated directory is never touched, except by
//! `self_clean_runs_at_process_exit`, which uses its own throwaway override
//! directory under `$HOME`.
//!
//! What these tests do NOT prove: that the prune is fail-closed on EVERY IO
//! error (only the observed shapes below), that `atexit` runs when the process
//! is killed (it does not; the prune is the backstop), or anything about
//! non-unix platforms.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::fs::{self, File, FileTimes};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const PARENT: &str = "blastguard-worktree-root-fixtures";
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

fn old() -> SystemTime {
    SystemTime::now() - DAY * 3
}

/// A fresh canonical `<scratch>/<case>/blastguard-worktree-root-fixtures`.
fn layout(case: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "fixture-base-prune-{case}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let root_real = {
        fs::create_dir_all(&root).unwrap();
        root.canonicalize().unwrap()
    };
    let parent = root_real.join(PARENT);
    fs::create_dir_all(&parent).unwrap();
    (root_real, parent)
}

fn age(path: &Path) {
    let f = File::open(path).unwrap();
    f.set_times(FileTimes::new().set_modified(old())).unwrap();
}

fn mkdir_with_file(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("payload"), "x").unwrap();
}

// `lutimes` sets a SYMLINK's own timestamps (std cannot). libc is linked by
// std already; declared by hand because libc is not a dependency of this crate
// (the module under test does the same for `atexit`).
#[repr(C)]
struct Timeval {
    tv_sec: i64,
    #[cfg(target_os = "macos")]
    tv_usec: i32,
    #[cfg(not(target_os = "macos"))]
    tv_usec: i64,
}
extern "C" {
    fn lutimes(path: *const std::os::raw::c_char, times: *const Timeval) -> std::os::raw::c_int;
}

fn age_symlink_itself(link: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(link.as_os_str().as_bytes()).unwrap();
    let secs = old()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let tv = [
        Timeval {
            tv_sec: secs,
            tv_usec: 0,
        },
        Timeval {
            tv_sec: secs,
            tv_usec: 0,
        },
    ];
    // SAFETY: valid NUL-terminated path and a 2-element timeval array.
    let rc = unsafe { lutimes(c.as_ptr(), tv.as_ptr()) };
    assert_eq!(rc, 0, "lutimes failed: {}", std::io::Error::last_os_error());
    let own = fs::symlink_metadata(link).unwrap().modified().unwrap();
    assert!(
        own.elapsed().unwrap() > DAY,
        "precondition: the symlink's OWN mtime must be stale, or the symlink case proves nothing"
    );
}

#[test]
fn prune_removes_only_stale_real_run_dirs() {
    let (root, parent) = layout("shapes");
    let stale = parent.join("run-1-1");
    let fresh = parent.join("run-2-2");
    let stale_other = parent.join("other-3-3");
    let stale_file = parent.join("run-4-4");
    for d in [&stale, &fresh, &stale_other] {
        mkdir_with_file(d);
    }
    fs::write(&stale_file, "not a dir").unwrap();
    age(&stale);
    age(&stale_other);
    age(&stale_file);

    neutral_base::prune_stale_runs(&parent);

    assert!(!stale.exists(), "stale real run-* dir must be removed");
    assert!(fresh.join("payload").exists(), "fresh run-* dir must stay");
    assert!(
        stale_other.join("payload").exists(),
        "stale dir without the run- prefix must stay"
    );
    assert!(stale_file.is_file(), "run-* regular file must stay");
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn prune_does_not_follow_a_stale_run_symlink() {
    let (root, parent) = layout("symlink");
    let target = root.join("outside-target");
    mkdir_with_file(&target);
    mkdir_with_file(&target.join("nested"));
    age(&target);
    let link = parent.join("run-5-5");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    age_symlink_itself(&link);

    neutral_base::prune_stale_runs(&parent);

    assert!(
        target.join("payload").exists() && target.join("nested/payload").exists(),
        "the symlink's target and its contents must survive the prune"
    );
    assert!(
        fs::symlink_metadata(&link).is_ok(),
        "the symlink itself is skipped, not removed"
    );
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn prune_refuses_a_parent_with_the_wrong_name_or_a_symlinked_spelling() {
    // (a) a stale run-* dir directly in a directory NOT named like the
    // dedicated one is out of scope.
    let (root, _parent) = layout("wrongname");
    let wrong = root.join("not-the-dedicated-dir");
    let victim = wrong.join("run-6-6");
    mkdir_with_file(&victim);
    age(&victim);
    neutral_base::prune_stale_runs(&wrong);
    assert!(
        victim.join("payload").exists(),
        "a parent not named {PARENT} must not be pruned"
    );

    // (b) the dedicated dir reached through a symlink spelling (non-canonical).
    let real = root.join("real");
    let real_parent = real.join(PARENT);
    let victim2 = real_parent.join("run-7-7");
    mkdir_with_file(&victim2);
    age(&victim2);
    let via = root.join("via");
    fs::create_dir_all(&via).unwrap();
    std::os::unix::fs::symlink(&real_parent, via.join(PARENT)).unwrap();
    neutral_base::prune_stale_runs(&via.join(PARENT));
    assert!(
        victim2.join("payload").exists(),
        "a non-canonical (symlinked) spelling of the dedicated dir must not be pruned"
    );
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn clean_own_run_removes_only_that_run_dir() {
    let (root, parent) = layout("selfclean");
    let own = parent.join("run-8-8");
    let sibling = parent.join("run-9-9");
    let bystander = parent.join("keep-me");
    for d in [&own, &sibling, &bystander] {
        mkdir_with_file(d);
    }
    mkdir_with_file(&own.join("nested"));

    neutral_base::clean_own_run(&own);

    assert!(
        !own.exists(),
        "own run dir (and its contents) must be removed"
    );
    assert!(
        sibling.join("payload").exists(),
        "sibling run dir must stay"
    );
    assert!(
        bystander.join("payload").exists(),
        "other entries must stay"
    );
    assert!(parent.is_dir(), "the dedicated dir itself must stay");
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn clean_own_run_refuses_a_symlink_a_non_run_name_and_a_foreign_parent() {
    let (root, parent) = layout("selfclean-refuse");
    // symlink named run-*: must not delete the target.
    let target = root.join("outside-target");
    mkdir_with_file(&target);
    let link = parent.join("run-10-10");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    neutral_base::clean_own_run(&link);
    assert!(
        target.join("payload").exists(),
        "symlink target must survive"
    );

    // not a run-* name.
    let named = parent.join("other-11-11");
    mkdir_with_file(&named);
    neutral_base::clean_own_run(&named);
    assert!(
        named.join("payload").exists(),
        "non run-* name must survive"
    );

    // run-* dir whose parent is not the dedicated dir.
    let foreign = root.join("foreign").join("run-12-12");
    mkdir_with_file(&foreign);
    neutral_base::clean_own_run(&foreign);
    assert!(
        foreign.join("payload").exists(),
        "foreign parent must survive"
    );
    fs::remove_dir_all(&root).unwrap();
}

const CHILD_ENV: &str = "FIXTURE_BASE_PRUNE_CHILD";

/// Entry point re-executed as a child by `self_clean_runs_at_process_exit`.
/// A no-op unless the child env is set. Prints the physical run dir.
#[test]
fn child_entry_creates_base() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let base = neutral_base::neutral_base();
    let real = base.canonicalize().unwrap();
    fs::write(real.join("marker"), "x").unwrap();
    println!("CHILD_RUN_DIR={}", real.display());
}

/// End to end: a real process that obtains `neutral_base()` registers the
/// `atexit` self-clean; after it exits its run dir is gone and a sibling run
/// dir planted in the same dedicated dir survives.
#[test]
fn self_clean_runs_at_process_exit() {
    let home = std::env::var_os("HOME").expect("HOME");
    let over = Path::new(&home).join(format!(
        ".blastguard-test-fixtures-selfclean-probe-{}",
        std::process::id()
    ));
    let parent = over.join(PARENT);
    fs::create_dir_all(&parent).unwrap();
    let sibling = parent.join("run-1-1");
    mkdir_with_file(&sibling);

    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["child_entry_creates_base", "--exact", "--nocapture"])
        .env(CHILD_ENV, "1")
        .env("BLASTGUARD_TEST_FIXTURE_DIR", &over)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let child_run = stdout
        .lines()
        .find_map(|l| l.strip_prefix("CHILD_RUN_DIR="))
        .map(PathBuf::from);
    let sibling_ok = sibling.join("payload").exists();
    let child_gone = child_run.as_ref().map(|p| !p.exists());
    let _ = fs::remove_dir_all(&over);

    assert!(
        out.status.success(),
        "child failed: {:?}\n{stdout}\n{stderr}",
        out.status
    );
    let child_run = child_run.unwrap_or_else(|| panic!("child printed no run dir:\n{stdout}"));
    assert!(
        child_run
            .parent()
            .is_some_and(|p| p.file_name().is_some_and(|n| n == PARENT)),
        "child's run dir {} is not directly in the dedicated dir",
        child_run.display()
    );
    assert_eq!(
        child_gone,
        Some(true),
        "child's run dir must be gone after exit"
    );
    assert!(sibling_ok, "sibling run dir must survive the child's exit");
}
