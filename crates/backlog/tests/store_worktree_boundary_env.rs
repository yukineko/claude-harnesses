//! Backlog 1e6f00ae, round 2: the primary-tree boundary must be decided by the
//! PATH, not by ambient git environment. `GIT_DIR` / `GIT_WORK_TREE` can make
//! `git rev-parse` describe a different repository or worktree than the one the
//! cwd actually lives in (cargo tests and the commands this repo runs from git
//! hooks have exactly that environment), so a classification that trusts git's
//! answer is fail-open. Also pins `cancel` / `fail` refusal and the end-to-end
//! Undetermined path (a primary whose `.git` points at a missing gitdir).
//!
//! Every spawned process has GIT_DIR / GIT_WORK_TREE / GIT_COMMON_DIR /
//! GIT_INDEX_FILE removed unless a test sets them on purpose, so a leaked
//! variable from an outer git hook cannot change what is measured.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const GIT_ENV: [&str; 4] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
];

struct Fixture {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    primary: PathBuf,
    linked: PathBuf,
    home: PathBuf,
    shim: PathBuf,
}

fn git(cwd: &Path, args: &[&str]) {
    let mut c = Command::new("git");
    c.args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null");
    for k in GIT_ENV {
        c.env_remove(k);
    }
    let out = c.output().expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn exec_shim(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn seeded_task(project: &Path) -> String {
    format!(
        "[[task]]\nid = \"seed0001\"\ntitle = \"seeded\"\nproject = \"{}\"\ntags = []\nstatus = \"pending\"\nnotes = \"\"\ncreated_at = 1000\nupdated_at = 1000\n\n",
        project.display()
    )
}

/// `seed`: commit a seeded tasks.toml into the primary before the worktree is cut.
fn fixture(seed: bool) -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let primary = base.join("primary");
    let home = base.join("home");
    let shim = base.join("shim");
    for d in [&primary, &home, &shim] {
        std::fs::create_dir_all(d).unwrap();
    }
    exec_shim(
        &shim.join("condukt"),
        "#!/bin/sh\necho '{\"claimed\":false}'\nexit 1\n",
    );
    exec_shim(
        &shim.join("gh"),
        "#!/bin/sh\necho \"$@\" >> \"$(dirname \"$0\")/gh.log\"\nif [ \"$1 $2\" = \"issue create\" ]; then echo https://github.com/o/r/issues/42; fi\nexit 0\n",
    );
    git(&primary, &["init", "-q", "-b", "main"]);
    git(
        &primary,
        &["remote", "add", "origin", "https://github.com/o/r.git"],
    );
    std::fs::write(primary.join("README"), "x\n").unwrap();
    if seed {
        std::fs::create_dir_all(primary.join(".backlog")).unwrap();
        std::fs::write(primary.join(".backlog/tasks.toml"), seeded_task(&primary)).unwrap();
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
        base,
        primary,
        linked,
        home,
        shim,
    }
}

fn run(fx: &Fixture, cwd: &Path, args: &[&str], envs: &[(&str, &Path)]) -> (i32, String, String) {
    let path = format!(
        "{}:{}",
        fx.shim.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut c = Command::new(env!("CARGO_BIN_EXE_backlog"));
    c.args(args)
        .env("HOME", &fx.home)
        .env("PATH", path)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("BACKLOG_DISABLE")
        .current_dir(cwd)
        .stdin(Stdio::null());
    for k in GIT_ENV {
        c.env_remove(k);
    }
    for (k, v) in envs {
        c.env(k, v);
    }
    let out = c.output().expect("backlog runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn store(root: &Path) -> PathBuf {
    root.join(".backlog").join("tasks.toml")
}

fn add_args(project: &Path) -> Vec<String> {
    vec![
        "add".into(),
        "--title".into(),
        "env-probe".into(),
        "--project".into(),
        project.to_str().unwrap().into(),
    ]
}

fn run_add(fx: &Fixture, cwd: &Path, envs: &[(&str, &Path)]) -> (i32, String, String) {
    let a = add_args(cwd);
    let a: Vec<&str> = a.iter().map(String::as_str).collect();
    run(fx, cwd, &a, envs)
}

fn seed_linked(fx: &Fixture) {
    std::fs::create_dir_all(fx.linked.join(".backlog")).unwrap();
    std::fs::write(store(&fx.linked), seeded_task(&fx.linked)).unwrap();
}

// ---- 1. GIT_DIR / GIT_WORK_TREE must not move the boundary ----------------

#[test]
fn git_dir_of_linked_worktree_does_not_unlock_a_primary_cwd() {
    let fx = fixture(false);
    let gitdir = fx.primary.join(".git/worktrees/linked");
    assert!(
        gitdir.is_dir(),
        "fixture precondition: {}",
        gitdir.display()
    );
    let (code, out, err) = run_add(&fx, &fx.primary, &[("GIT_DIR", &gitdir)]);
    assert_ne!(
        code, 0,
        "GIT_DIR must not unlock a primary cwd; out={out:?} err={err:?}"
    );
    assert!(
        !fx.primary.join(".backlog").exists(),
        "nothing may be written into the primary tree; out={out:?} err={err:?}"
    );
}

#[test]
fn git_dir_and_work_tree_of_linked_worktree_do_not_unlock_a_primary_cwd() {
    let fx = fixture(false);
    let gitdir = fx.primary.join(".git/worktrees/linked");
    let (code, out, err) = run_add(
        &fx,
        &fx.primary,
        &[("GIT_DIR", &gitdir), ("GIT_WORK_TREE", &fx.linked)],
    );
    assert_ne!(
        code, 0,
        "GIT_DIR+GIT_WORK_TREE must not unlock a primary cwd; out={out:?} err={err:?}"
    );
    assert!(
        !fx.primary.join(".backlog").exists(),
        "nothing may be written into the primary tree; out={out:?} err={err:?}"
    );
}

#[test]
fn git_dir_of_a_separate_bare_repo_does_not_unlock_a_primary_cwd() {
    let fx = fixture(false);
    let bare = fx.base.join("bare.git");
    git(
        &fx.base,
        &[
            "clone",
            "-q",
            "--bare",
            fx.primary.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    let (code, out, err) = run_add(&fx, &fx.primary, &[("GIT_DIR", &bare)]);
    assert_ne!(
        code, 0,
        "bare GIT_DIR must not unlock a primary cwd; out={out:?} err={err:?}"
    );
    assert!(
        !fx.primary.join(".backlog").exists(),
        "nothing may be written into the primary tree; out={out:?} err={err:?}"
    );
}

/// Control: the PATH decides. A linked-worktree cwd stays writable even when
/// GIT_DIR names the primary's git dir.
#[test]
fn git_dir_of_primary_does_not_lock_a_linked_worktree_cwd() {
    let fx = fixture(false);
    let gitdir = fx.primary.join(".git");
    let (code, out, err) = run_add(&fx, &fx.linked, &[("GIT_DIR", &gitdir)]);
    assert_eq!(
        code, 0,
        "a linked-worktree cwd must stay writable regardless of GIT_DIR; out={out:?} err={err:?}"
    );
    let body = std::fs::read_to_string(store(&fx.linked)).expect("store written in the worktree");
    assert!(body.contains("env-probe"), "{body}");
    assert!(!fx.primary.join(".backlog").exists());
}

// ---- 2. cancel / fail ------------------------------------------------------

#[test]
fn cancel_from_primary_checkout_is_refused_and_file_unchanged() {
    let fx = fixture(true);
    let before = std::fs::read(store(&fx.primary)).unwrap();
    let (code, out, err) = run(
        &fx,
        &fx.primary,
        &["cancel", "seed0001", "--reason", "not needed"],
        &[],
    );
    assert_ne!(
        code, 0,
        "cancel from primary must be refused; out={out:?} err={err:?}"
    );
    assert_eq!(
        before,
        std::fs::read(store(&fx.primary)).unwrap(),
        "refused cancel must leave the store byte-identical; out={out:?} err={err:?}"
    );
}

#[test]
fn fail_from_primary_checkout_is_refused_and_file_unchanged() {
    let fx = fixture(true);
    let before = std::fs::read(store(&fx.primary)).unwrap();
    let (code, out, err) = run(
        &fx,
        &fx.primary,
        &["fail", "seed0001", "--reason", "broke"],
        &[],
    );
    assert_ne!(
        code, 0,
        "fail from primary must be refused; out={out:?} err={err:?}"
    );
    assert_eq!(
        before,
        std::fs::read(store(&fx.primary)).unwrap(),
        "refused fail must leave the store byte-identical; out={out:?} err={err:?}"
    );
}

#[test]
fn cancel_from_linked_worktree_succeeds_and_primary_is_untouched() {
    let fx = fixture(true);
    seed_linked(&fx);
    let main_before = std::fs::read(store(&fx.primary)).unwrap();
    let before = std::fs::read(store(&fx.linked)).unwrap();
    let (code, out, err) = run(
        &fx,
        &fx.linked,
        &["cancel", "seed0001", "--reason", "not needed"],
        &[],
    );
    assert_eq!(code, 0, "out={out:?} err={err:?}");
    assert_ne!(
        before,
        std::fs::read(store(&fx.linked)).unwrap(),
        "worktree store must change"
    );
    assert_eq!(main_before, std::fs::read(store(&fx.primary)).unwrap());
}

#[test]
fn fail_from_linked_worktree_succeeds_and_primary_is_untouched() {
    let fx = fixture(true);
    seed_linked(&fx);
    let main_before = std::fs::read(store(&fx.primary)).unwrap();
    let before = std::fs::read(store(&fx.linked)).unwrap();
    let (code, out, err) = run(
        &fx,
        &fx.linked,
        &["fail", "seed0001", "--reason", "broke"],
        &[],
    );
    assert_eq!(code, 0, "out={out:?} err={err:?}");
    assert_ne!(
        before,
        std::fs::read(store(&fx.linked)).unwrap(),
        "worktree store must change"
    );
    assert_eq!(main_before, std::fs::read(store(&fx.primary)).unwrap());
}

// ---- 5. Undetermined, end to end -------------------------------------------

/// A "primary" whose `.git` is a FILE pointing at a gitdir that does not exist:
/// the boundary cannot be classified, so a write must be refused (an
/// undetermined boundary is not a linked worktree).
#[test]
fn add_where_dot_git_points_at_a_missing_gitdir_is_refused() {
    let fx = fixture(false);
    let broken = fx.base.join("broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(
        broken.join(".git"),
        format!("gitdir: {}\n", fx.base.join("no-such-gitdir").display()),
    )
    .unwrap();
    let (code, out, err) = run_add(&fx, &broken, &[]);
    assert_ne!(
        code, 0,
        "an undetermined boundary must be refused; out={out:?} err={err:?}"
    );
    assert!(
        err.contains("could not determine"),
        "refusal must be for the undetermined boundary, not an unrelated error; stderr={err:?}"
    );
    assert!(
        !broken.join(".backlog").exists(),
        "nothing may be written when the boundary is undetermined; out={out:?} err={err:?}"
    );
}
