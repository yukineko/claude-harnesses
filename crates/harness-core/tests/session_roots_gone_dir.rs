// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (user ruling 2026-10-27): a written ABSOLUTE path whose parent
//! directory no longer exists, with no worktree-origin record covering it,
//! makes `session_roots::resolve_with_store` Undetermined, so donegate and tdd
//! block every Stop of the session.
//!
//! New contract: walk up to the nearest EXISTING ancestor and ask git there.
//! - exit 128 with the full "not a git repository (or any of the parent
//!   directories)" -> positively outside every live repo -> EXCLUDED (no root,
//!   no error, still counts toward `wrote_any`).
//! - the ancestor is inside a repo -> that repo's toplevel becomes a root.
//! - every other failure stays Undetermined: relative path, ancestor git cannot
//!   enter (mode 000), corrupt gitfile in the ancestor, git missing, ...
//!
//! RED now: `removed_worktree_without_record_under_non_repo_base_is_excluded`,
//! `deleted_subdir_of_live_plain_repo_resolves_to_the_repo`.
//! Controls (Undetermined before and after): `control_*`.
//!
//! Every test passes an explicit private store, so the real ~/.harness store is
//! never read or written. Written by a disinterested author (CLAUDE.md 2(a)).

use harness_core::session_roots::resolve_with_store;
use harness_core::verdict::Determination;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    harness_core::git_env::scrub(&mut Command::new("git"))
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git must be runnable for this test")
}

fn ok(out: std::process::Output) {
    assert!(out.status.success(), "git failed: {out:?}");
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap()
}

/// Precondition: `dir` really is outside every repo (exit 128 + full phrase).
/// Panics (never skips) when the environment breaks it.
fn assert_positively_outside(dir: &Path) {
    let out = git(dir, &["rev-parse", "--show-toplevel"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(128),
        "precondition: {dir:?} {out:?}"
    );
    assert!(
        err.contains("not a git repository (or any of the parent directories)"),
        "precondition: {dir:?} must be outside every repo, git said: {err}"
    );
}

fn init_repo_with_commit(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    ok(git(dir, &["init", "-q"]));
    ok(git(
        dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    ));
}

/// JSONL transcript recording a `Write` of each path; returns its path.
fn transcript(dir: &Path, written: &[&str]) -> String {
    let mut body = String::new();
    for (i, p) in written.iter().enumerate() {
        let line = serde_json::json!({
            "message": {"role": "assistant", "content": [{
                "type": "tool_use", "id": format!("toolu_{i}"), "name": "Write",
                "input": {"file_path": p, "content": "x"}
            }]}
        });
        body.push_str(&line.to_string());
        body.push('\n');
    }
    let path = dir.join("session.jsonl");
    std::fs::write(&path, body).unwrap();
    path.to_string_lossy().into_owned()
}

/// `<tmp>` (positively outside every repo) with `cwd/` (non-repo hook cwd),
/// `t/` (transcript dir) and a private, empty worktree-origin store path.
struct Fixture {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    cwd: PathBuf,
    tdir: PathBuf,
    store: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let base = canon(tmp.path());
    assert_positively_outside(&base);
    let cwd = base.join("cwd");
    let tdir = base.join("t");
    for d in [&cwd, &tdir] {
        std::fs::create_dir_all(d).unwrap();
    }
    assert_positively_outside(&cwd);
    let store = base
        .join("store")
        .join(harness_core::worktree_origin::FILE_NAME);
    Fixture {
        _tmp: tmp,
        base,
        cwd,
        tdir,
        store,
    }
}

impl Fixture {
    fn resolve(
        &self,
        written: &[&str],
    ) -> Determination<harness_core::session_roots::SessionRoots> {
        let t = transcript(&self.tdir, written);
        resolve_with_store(&self.cwd, &t, &self.store)
    }
}

// ── RED: must FAIL until the fix lands ───────────────────────────────────────

/// Case 1: a linked worktree under a non-repo base was removed before any
/// resolve saw it (no store record). The path is outside every LIVE repo, so
/// it is excluded; the roots are the hook cwd only.
#[test]
fn removed_worktree_without_record_under_non_repo_base_is_excluded() {
    let f = fixture();
    let repo = f.base.join("repo");
    let wt = f.base.join("wt");
    init_repo_with_commit(&repo);
    ok(git(
        &repo,
        &["worktree", "add", "-q", "-b", "w", wt.to_str().unwrap()],
    ));
    let written = wt.join("src").join("a.rs");
    std::fs::create_dir_all(written.parent().unwrap()).unwrap();
    std::fs::write(&written, "x").unwrap();
    ok(git(
        &repo,
        &["worktree", "remove", "--force", wt.to_str().unwrap()],
    ));
    assert!(!wt.exists(), "precondition: the worktree dir must be gone");
    assert!(!f.store.exists(), "precondition: no store record exists");

    match f.resolve(&[written.to_str().unwrap()]) {
        Determination::Known(sr) => {
            assert!(
                !sr.roots.iter().any(|r| r.starts_with(&wt)),
                "the removed path must not be a root: {sr:?}"
            );
            assert_eq!(
                sr.roots,
                vec![canon(&f.cwd)],
                "roots must be the hook cwd only (the repo was never written to)"
            );
            assert!(sr.wrote_any, "the session did write a file");
        }
        Determination::Undetermined(why) => panic!(
            "a write under a non-repo base whose directory is gone must be excluded, \
             not Undetermined: {}",
            why.as_str()
        ),
    }
}

/// Case 2: a deleted subdirectory inside a live (non-linked) repo. The nearest
/// existing ancestor is inside the repo, so the repo toplevel is a root. The
/// cwd is a separate non-repo dir, so `contains(repo)` is not vacuous.
#[test]
fn deleted_subdir_of_live_plain_repo_resolves_to_the_repo() {
    let f = fixture();
    let repo = f.base.join("repo");
    init_repo_with_commit(&repo);
    let written = repo.join("sub").join("deep").join("a.rs");
    std::fs::create_dir_all(written.parent().unwrap()).unwrap();
    std::fs::write(&written, "x").unwrap();
    std::fs::remove_dir_all(repo.join("sub")).unwrap();
    assert!(repo.is_dir() && !repo.join("sub").exists(), "precondition");

    match f.resolve(&[written.to_str().unwrap()]) {
        Determination::Known(sr) => {
            assert!(
                sr.roots.contains(&canon(&repo)),
                "the live repo containing the deleted dir must be a root: {sr:?}"
            );
            assert!(sr.wrote_any);
        }
        Determination::Undetermined(why) => panic!(
            "the nearest existing ancestor is inside a live repo; roots must be Known: {}",
            why.as_str()
        ),
    }
}

// ── controls: Undetermined now and after the fix ─────────────────────────────

/// A relative path never qualifies, whether or not its directory exists.
#[test]
fn control_relative_missing_dir_stays_undetermined() {
    let f = fixture();
    let rel = Path::new("gone").join("x.rs");
    assert!(!rel.is_absolute() && !rel.parent().unwrap().exists());
    let got = f.resolve(&[rel.to_str().unwrap()]);
    assert!(
        matches!(got, Determination::Undetermined(_)),
        "a relative path with a missing dir is not positively outside: {got:?}"
    );
}

/// Missing dir whose nearest existing ancestor git cannot enter (mode 000).
#[cfg(unix)]
#[test]
fn control_missing_dir_under_unenterable_ancestor_stays_undetermined() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let locked = f.base.join("locked");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let blocked = std::fs::read_dir(&locked).is_err();
    let file = locked.join("gone").join("x.rs");
    let got = f.resolve(&[file.to_str().unwrap()]);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        blocked,
        "precondition: mode 000 must be enforced (not running as root)"
    );
    assert!(
        matches!(got, Determination::Undetermined(_)),
        "an ancestor git cannot enter is not 'outside every repo': {got:?}"
    );
}

/// Missing dir whose nearest existing ancestor holds a corrupt `.git` gitfile:
/// git says "not a git repository" WITHOUT the parenthetical.
#[test]
fn control_missing_dir_under_corrupt_gitfile_ancestor_stays_undetermined() {
    let f = fixture();
    let broken = f.base.join("broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join(".git"), "gitdir: /nonexistent/harness-test\n").unwrap();
    let out = git(&broken, &["rev-parse", "--show-toplevel"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "precondition: {out:?}");
    assert!(
        !err.contains("(or any of the parent directories)"),
        "precondition: corrupt gitfile must not look like 'outside': {err}"
    );
    let file = broken.join("gone").join("x.rs");
    let got = f.resolve(&[file.to_str().unwrap()]);
    assert!(
        matches!(got, Determination::Undetermined(_)),
        "a corrupt checkout ancestor is not 'outside every repo': {got:?}"
    );
}
