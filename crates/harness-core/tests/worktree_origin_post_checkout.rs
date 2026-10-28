//! The `.githooks/post-checkout` recorder: a linked worktree created through
//! `git worktree add` must be recorded in the worktree-origin store so a write
//! into it still resolves after the worktree is removed.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use harness_core::session_roots::resolve_with_store;
use harness_core::verdict::Determination;
use harness_core::worktree_origin::{DIR_ENV, FILE_NAME};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn hook_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.githooks/post-checkout")
}

fn git_cmd(dir: &Path, origin_dir: Option<&Path>, args: &[&str]) -> Output {
    let mut c = Command::new("git");
    for k in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_CEILING_DIRECTORIES",
    ] {
        c.env_remove(k);
    }
    match origin_dir {
        Some(d) => c.env(DIR_ENV, d),
        None => c.env_remove(DIR_ENV),
    };
    c.arg("-C").arg(dir).args(args).output().unwrap()
}

fn git_ok(dir: &Path, origin_dir: Option<&Path>, args: &[&str]) {
    let o = git_cmd(dir, origin_dir, args);
    assert!(o.status.success(), "git {args:?}: {o:?}");
}

struct Fx {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    repo: PathBuf,
    wt: PathBuf,
}

fn fixture(install_hook: bool) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let repo = base.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git_ok(&repo, None, &["init", "-q"]);
    git_ok(
        &repo,
        None,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@e",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "i",
        ],
    );
    if install_hook {
        let src = hook_src();
        assert!(
            src.is_file(),
            "the recorder hook {} does not exist",
            src.display()
        );
        let dst = repo.join(".git/hooks/post-checkout");
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        std::fs::copy(&src, &dst).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let wt = base.join("wt");
    Fx {
        _tmp: tmp,
        base,
        repo,
        wt,
    }
}

/// Add the worktree, write `src/a.rs`, build a transcript, remove the worktree.
/// Returns (add output, transcript path). No resolve happens here.
fn add_write_remove(fx: &Fx, origin_dir: &Path) -> (Output, String) {
    let add = git_cmd(
        &fx.repo,
        Some(origin_dir),
        &["worktree", "add", "-q", "-b", "w", fx.wt.to_str().unwrap()],
    );
    assert!(add.status.success(), "git worktree add: {add:?}");
    assert!(fx.wt.is_dir());
    let written = fx.wt.join("src").join("a.rs");
    std::fs::create_dir_all(written.parent().unwrap()).unwrap();
    std::fs::write(&written, "x").unwrap();
    let line = serde_json::json!({
        "message": {"role": "assistant", "content": [{
            "type": "tool_use", "id": "toolu_0", "name": "Write",
            "input": {"file_path": written.to_str().unwrap(), "content": "x"}
        }]}
    });
    let t = fx.base.join("session.jsonl");
    std::fs::write(&t, format!("{line}\n")).unwrap();
    git_ok(
        &fx.repo,
        Some(origin_dir),
        &["worktree", "remove", "--force", fx.wt.to_str().unwrap()],
    );
    assert!(!fx.wt.exists());
    (add, t.to_string_lossy().into_owned())
}

#[test]
fn worktree_created_with_recorder_resolves_after_removal() {
    let fx = fixture(true);
    let origin_dir = fx.base.join("origins");
    let (_, transcript) = add_write_remove(&fx, &origin_dir);
    let store = origin_dir.join(FILE_NAME);
    match resolve_with_store(&fx.repo, &transcript, &store) {
        Determination::Known(sr) => assert!(sr.roots.contains(&fx.repo), "{sr:?}"),
        Determination::Undetermined(w) => panic!("expected Known: {}", w.as_str()),
    }
}

// user ruling 2026-10-27: without a recorder there is still no record, but a
// gone directory under a non-repo base is now excluded (Known), not Undetermined.
// The repo is the hook cwd here, so it is a root only as the cwd.
#[test]
fn worktree_created_without_recorder_gone_dir_is_excluded() {
    let fx = fixture(false);
    let origin_dir = fx.base.join("origins");
    let (_, transcript) = add_write_remove(&fx, &origin_dir);
    let store = origin_dir.join(FILE_NAME);
    assert!(!store.exists(), "precondition: no record was written");
    match resolve_with_store(&fx.repo, &transcript, &store) {
        Determination::Known(sr) => {
            assert_eq!(sr.roots, vec![fx.repo.clone()], "{sr:?}");
            assert!(sr.wrote_any);
        }
        Determination::Undetermined(w) => panic!("expected Known: {}", w.as_str()),
    }
}

#[test]
fn recorder_failure_does_not_fail_worktree_add_and_records_nothing() {
    let fx = fixture(true);
    // The store directory's parent is a regular file: it cannot be created.
    let blocker = fx.base.join("blocker");
    std::fs::write(&blocker, "file").unwrap();
    let bad_dir = blocker.join("origins");
    let (add, transcript) = add_write_remove(&fx, &bad_dir);
    assert!(add.status.success(), "{add:?}");
    assert!(!bad_dir.exists());
    let other_store = fx.base.join("other").join(FILE_NAME);
    // The real property: the failed recorder wrote no record anywhere.
    assert!(!other_store.exists(), "no record may exist in other_store");
    // user ruling 2026-10-27: a removed, unrecorded worktree under a non-repo
    // base is excluded (Known, roots == [cwd]), no longer Undetermined.
    match resolve_with_store(&fx.repo, &transcript, &other_store) {
        Determination::Known(sr) => {
            assert_eq!(sr.roots, vec![fx.repo.clone()], "{sr:?}");
            assert!(sr.wrote_any);
        }
        Determination::Undetermined(w) => panic!("expected Known: {}", w.as_str()),
    }
    assert!(
        !other_store.exists(),
        "resolving a removed worktree must not create a record"
    );
}

#[test]
fn main_checkout_records_nothing() {
    let fx = fixture(true);
    let origin_dir = fx.base.join("origins");
    git_ok(
        &fx.repo,
        Some(&origin_dir),
        &["checkout", "-q", "-b", "other"],
    );
    assert!(
        !origin_dir.join(FILE_NAME).exists(),
        "a primary-tree checkout must not record"
    );
}
