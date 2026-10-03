//! Backlog 1e6f00ae: a compass state-store WRITE whose resolved store lies
//! inside the PRIMARY working tree of a git repository is refused (CLAUDE.md
//! §8). Writes from a LINKED worktree succeed, reads from the primary tree stay
//! allowed, and a cwd with no git repo above it keeps its current behaviour.
//!
//! compass resolves its store as `<cwd>/.compass/...` (`project_root()` is
//! `std::env::current_dir()`, not the git root), so a subdirectory of the
//! primary checkout is covered too: the boundary is "inside the primary tree",
//! not "at its root".
//!
//! Refusal tests assert on the files (existence / exact bytes), not only the
//! exit code.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const CHARTER_JSON: &str = r#"{"north_star":"ship it","definition_of_done":["tests pass"],"measuring_stick":"tests passing","current_gap":"gap","next_action":"act","parked":[]}"#;

struct Fixture {
    _tmp: tempfile::TempDir,
    primary: PathBuf,
    linked: PathBuf,
    home: PathBuf,
    shim: PathBuf,
}

fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `with_charter`: commit a charter into the repo before the worktree is cut,
/// so `outcome` / `gap --write` have one in BOTH checkouts.
fn fixture(with_charter: bool) -> Fixture {
    fixture_with(with_charter, false)
}

/// `with_carve`: also commit a `.compass/carve-state.json` (made by `evaluate`
/// in a scratch non-git dir) so `apply` / `evaluate` / `carve-reset` have an
/// existing state to clobber or delete.
fn fixture_with(with_charter: bool, with_carve: bool) -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let primary = base.join("primary");
    let home = base.join("home");
    let shim = base.join("shim");
    for d in [&primary, &home, &shim] {
        std::fs::create_dir_all(d).unwrap();
    }
    let condukt = shim.join("condukt");
    std::fs::write(&condukt, "#!/bin/sh\nexit 1\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&condukt, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    git(&primary, &["init", "-q", "-b", "main"]);
    std::fs::write(primary.join("README"), "x\n").unwrap();
    std::fs::create_dir_all(primary.join("sub")).unwrap();
    std::fs::write(primary.join("sub/f"), "x\n").unwrap();
    if with_charter {
        // Seed via a scratch NON-git dir (the control case) and copy the file in,
        // so fixture setup never depends on writing through the boundary under test.
        let scratch = base.join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let (code, _, err) = run_raw(
            &home,
            &shim,
            &scratch,
            &["charter", "--write", CHARTER_JSON],
        );
        assert_eq!(code, 0, "fixture charter seed failed: {err}");
        std::fs::create_dir_all(primary.join(".compass")).unwrap();
        std::fs::copy(charter(&scratch), charter(&primary)).unwrap();
    }
    if with_carve {
        let scratch = base.join("scratch-carve");
        std::fs::create_dir_all(&scratch).unwrap();
        let (code, _, err) = run_raw(&home, &shim, &scratch, &["evaluate"]);
        assert_eq!(code, 0, "fixture carve-state seed failed: {err}");
        std::fs::create_dir_all(primary.join(".compass")).unwrap();
        std::fs::copy(carve_state(&scratch), carve_state(&primary)).unwrap();
    }
    git(&primary, &["add", "-A"]);
    git(&primary, &["commit", "-q", "-m", "init"]);
    let linked = base.join("linked");
    git(
        &primary,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            linked.to_str().unwrap(),
        ],
    );
    Fixture {
        _tmp: tmp,
        primary,
        linked,
        home,
        shim,
    }
}

fn run_raw(home: &Path, shim: &Path, cwd: &Path, args: &[&str]) -> (i32, String, String) {
    let path = format!(
        "{}:{}",
        shim.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new(env!("CARGO_BIN_EXE_compass"))
        .args(args)
        .env("HOME", home)
        .env("PATH", path)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .expect("compass runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn run(fx: &Fixture, cwd: &Path, args: &[&str]) -> (i32, String, String) {
    run_raw(&fx.home, &fx.shim, cwd, args)
}

fn charter(root: &Path) -> PathBuf {
    root.join(".compass").join("charter.md")
}

#[test]
fn charter_write_from_primary_checkout_is_refused_and_writes_nothing() {
    let fx = fixture(false);
    let (code, out, err) = run(&fx, &fx.primary, &["charter", "--write", CHARTER_JSON]);
    assert_ne!(
        code, 0,
        "write from primary must be refused; out={out:?} err={err:?}"
    );
    assert!(
        !fx.primary.join(".compass").exists(),
        "refused charter --write must not create .compass/ in the primary tree; out={out:?} err={err:?}"
    );
    assert!(
        err.to_lowercase().contains("worktree"),
        "the error must tell the caller to run from a linked worktree; stderr={err:?}"
    );
}

#[test]
fn charter_write_from_primary_subdirectory_is_refused_and_writes_nothing() {
    let fx = fixture(false);
    let sub = fx.primary.join("sub");
    let (code, out, err) = run(&fx, &sub, &["charter", "--write", CHARTER_JSON]);
    assert_ne!(
        code, 0,
        "write from a primary subdir must be refused; out={out:?} err={err:?}"
    );
    assert!(
        !sub.join(".compass").exists() && !fx.primary.join(".compass").exists(),
        "no .compass/ may appear anywhere in the primary tree; out={out:?} err={err:?}"
    );
}

#[test]
fn charter_write_from_linked_worktree_succeeds_there_only() {
    let fx = fixture(false);
    let (code, out, err) = run(&fx, &fx.linked, &["charter", "--write", CHARTER_JSON]);
    assert_eq!(
        code, 0,
        "linked-worktree write must succeed; out={out:?} err={err:?}"
    );
    let body = std::fs::read_to_string(charter(&fx.linked)).expect("charter written in worktree");
    assert!(body.contains("ship it"), "{body}");
    assert!(
        !fx.primary.join(".compass").exists(),
        "the primary tree must stay untouched by a worktree write"
    );
}

#[test]
fn outcome_from_primary_checkout_is_refused_and_writes_nothing() {
    let fx = fixture(true);
    let (code, out, err) = run(
        &fx,
        &fx.primary,
        &["outcome", "--verdict", "forward", "--evidence", "ran tests"],
    );
    assert_ne!(
        code, 0,
        "outcome from primary must be refused; out={out:?} err={err:?}"
    );
    assert!(
        !fx.primary.join(".compass/outcomes.json").exists(),
        "refused outcome must not create outcomes.json in the primary tree; out={out:?} err={err:?}"
    );
}

#[test]
fn outcome_from_linked_worktree_succeeds_there_only() {
    let fx = fixture(true);
    let (code, out, err) = run(
        &fx,
        &fx.linked,
        &["outcome", "--verdict", "forward", "--evidence", "ran tests"],
    );
    assert_eq!(
        code, 0,
        "outcome from a linked worktree must succeed; out={out:?} err={err:?}"
    );
    assert!(fx.linked.join(".compass/outcomes.json").exists());
    assert!(!fx.primary.join(".compass/outcomes.json").exists());
}

#[test]
fn gap_write_from_primary_checkout_is_refused_and_charter_unchanged() {
    let fx = fixture(true);
    let before = std::fs::read(charter(&fx.primary)).unwrap();
    let (code, out, err) = run(&fx, &fx.primary, &["gap", "--write", "a brand new gap"]);
    assert_ne!(
        code, 0,
        "gap --write from primary must be refused; out={out:?} err={err:?}"
    );
    assert_eq!(
        before,
        std::fs::read(charter(&fx.primary)).unwrap(),
        "refused gap --write must leave the charter byte-identical; out={out:?} err={err:?}"
    );
}

#[test]
fn opportunity_add_from_primary_checkout_is_refused_and_writes_nothing() {
    let fx = fixture(true);
    let (code, out, err) = run(&fx, &fx.primary, &["opportunity", "add", "--title", "bet"]);
    // Whatever the flag spelling, nothing may land in the primary tree.
    let _ = (&out, &err);
    assert_ne!(
        code, 0,
        "opportunity add from primary must be refused; out={out:?} err={err:?}"
    );
    assert!(
        !fx.primary.join(".compass/opportunities.json").exists(),
        "refused opportunity add must not write into the primary tree"
    );
}

#[test]
fn charter_show_from_primary_checkout_still_works() {
    let fx = fixture(true);
    let before = std::fs::read(charter(&fx.primary)).unwrap();
    let (code, out, err) = run(&fx, &fx.primary, &["charter"]);
    assert_eq!(
        code, 0,
        "reads from the primary tree stay allowed; out={out:?} err={err:?}"
    );
    assert!(out.contains("ship it"), "{out:?}");
    assert_eq!(before, std::fs::read(charter(&fx.primary)).unwrap());
}

#[test]
fn pivot_check_from_primary_checkout_still_works_and_creates_nothing() {
    let fx = fixture(false);
    let (code, out, err) = run(&fx, &fx.primary, &["pivot-check"]);
    assert_eq!(code, 0, "out={out:?} err={err:?}");
    assert!(out.contains("persevere"), "{out:?}");
    assert!(!fx.primary.join(".compass").exists());
}

/// Control: no git repo above the cwd. Observed today: compass writes to
/// `<cwd>/.compass/charter.md` and exits 0. The fix must not change this.
#[test]
fn charter_write_outside_any_git_repo_keeps_working() {
    let fx = fixture(false);
    let nogit = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(nogit.path()).unwrap();
    let (code, out, err) = run(&fx, &cwd, &["charter", "--write", CHARTER_JSON]);
    assert_eq!(
        code, 0,
        "non-git cwd keeps working; out={out:?} err={err:?}"
    );
    let body = std::fs::read_to_string(charter(&cwd)).expect("charter written under cwd");
    assert!(body.contains("ship it"), "{body}");
}

fn carve_state(root: &Path) -> PathBuf {
    root.join(".compass").join("carve-state.json")
}

const ANSWER: &str =
    r#"{"gate":"c1","reference":"north_star","value":"a sharper star","defer":false}"#;

#[test]
fn evaluate_from_primary_checkout_is_refused_and_writes_nothing() {
    let fx = fixture(false);
    let (code, out, err) = run(&fx, &fx.primary, &["evaluate"]);
    assert_ne!(
        code, 0,
        "evaluate from primary must be refused; out={out:?} err={err:?}"
    );
    assert!(
        !fx.primary.join(".compass").exists(),
        "refused evaluate must not create carve-state.json; out={out:?} err={err:?}"
    );
}

#[test]
fn evaluate_from_primary_checkout_leaves_existing_carve_state_unchanged() {
    let fx = fixture_with(false, true);
    let before = std::fs::read(carve_state(&fx.primary)).unwrap();
    let (code, out, err) = run(&fx, &fx.primary, &["evaluate"]);
    assert_ne!(code, 0, "out={out:?} err={err:?}");
    assert_eq!(
        before,
        std::fs::read(carve_state(&fx.primary)).unwrap(),
        "refused evaluate must leave the existing carve state byte-identical; out={out:?} err={err:?}"
    );
}

#[test]
fn evaluate_from_linked_worktree_succeeds_there_only() {
    let fx = fixture(false);
    let (code, out, err) = run(&fx, &fx.linked, &["evaluate"]);
    assert_eq!(
        code, 0,
        "evaluate from a linked worktree must succeed; out={out:?} err={err:?}"
    );
    assert!(carve_state(&fx.linked).exists());
    assert!(!fx.primary.join(".compass").exists());
}

#[test]
fn apply_from_primary_checkout_is_refused_and_writes_nothing() {
    let fx = fixture(false);
    let (code, out, err) = run(&fx, &fx.primary, &["apply", "--answer", ANSWER]);
    assert_ne!(
        code, 0,
        "apply from primary must be refused; out={out:?} err={err:?}"
    );
    assert!(
        !fx.primary.join(".compass").exists(),
        "refused apply must not create carve-state.json; out={out:?} err={err:?}"
    );
}

#[test]
fn apply_from_primary_checkout_leaves_existing_carve_state_unchanged() {
    let fx = fixture_with(false, true);
    let before = std::fs::read(carve_state(&fx.primary)).unwrap();
    let (code, out, err) = run(&fx, &fx.primary, &["apply", "--answer", ANSWER]);
    assert_ne!(code, 0, "out={out:?} err={err:?}");
    assert_eq!(
        before,
        std::fs::read(carve_state(&fx.primary)).unwrap(),
        "refused apply must leave the existing carve state byte-identical; out={out:?} err={err:?}"
    );
}

#[test]
fn apply_from_linked_worktree_succeeds_there_only() {
    let fx = fixture(false);
    let (code, out, err) = run(&fx, &fx.linked, &["apply", "--answer", ANSWER]);
    assert_eq!(
        code, 0,
        "apply from a linked worktree must succeed; out={out:?} err={err:?}"
    );
    assert!(carve_state(&fx.linked).exists());
    assert!(!fx.primary.join(".compass").exists());
}

#[test]
fn carve_reset_from_primary_checkout_is_refused_and_keeps_carve_state() {
    let fx = fixture_with(false, true);
    let before = std::fs::read(carve_state(&fx.primary)).unwrap();
    let (code, out, err) = run(&fx, &fx.primary, &["carve-reset"]);
    assert_ne!(
        code, 0,
        "carve-reset from primary must be refused; out={out:?} err={err:?}"
    );
    assert!(
        carve_state(&fx.primary).exists(),
        "refused carve-reset must not delete carve-state.json; out={out:?} err={err:?}"
    );
    assert_eq!(before, std::fs::read(carve_state(&fx.primary)).unwrap());
}

#[test]
fn carve_reset_from_linked_worktree_succeeds_and_deletes_there_only() {
    let fx = fixture_with(false, true);
    assert!(carve_state(&fx.linked).exists(), "fixture precondition");
    let (code, out, err) = run(&fx, &fx.linked, &["carve-reset"]);
    assert_eq!(
        code, 0,
        "carve-reset from a linked worktree must succeed; out={out:?} err={err:?}"
    );
    assert!(!carve_state(&fx.linked).exists(), "worktree state deleted");
    assert!(
        carve_state(&fx.primary).exists(),
        "primary state must survive a worktree reset"
    );
}
