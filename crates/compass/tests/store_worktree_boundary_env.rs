//! Backlog 1e6f00ae, round 2 (compass side): the primary-tree boundary is
//! decided by the PATH, not by ambient git environment (`GIT_DIR` /
//! `GIT_WORK_TREE`), a cwd inside `<primary>/.git` is not a free pass, `route`
//! (which appends to `.claude/progress.md`) is covered, and an undetermined
//! boundary (a `.git` file pointing at a missing gitdir) is refused.
//!
//! Every spawned process has GIT_DIR / GIT_WORK_TREE / GIT_COMMON_DIR /
//! GIT_INDEX_FILE removed unless a test sets them on purpose.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const GIT_ENV: [&str; 4] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
];
const CHARTER_JSON: &str = r#"{"north_star":"ship it","definition_of_done":["tests pass"],"measuring_stick":"tests passing","current_gap":"gap","next_action":"act","parked":[]}"#;
const DECOMPOSITION: &str = r#"{"goal":"g","tasks":[
 {"id":"a","title":"small-move","touched_files":[],"deps":[],"class":"serial","done_criteria":"d","size":"s"},
 {"id":"b","title":"parked-big-move","touched_files":[],"deps":[],"class":"serial","done_criteria":"d","size":"xl"}]}"#;

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

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let primary = base.join("primary");
    let home = base.join("home");
    let shim = base.join("shim");
    for d in [&primary, &home, &shim] {
        std::fs::create_dir_all(d).unwrap();
    }
    for (name, body) in [
        ("condukt", "#!/bin/sh\nexit 1\n"),
        ("gh", "#!/bin/sh\nexit 0\n"),
    ] {
        let p = shim.join(name);
        std::fs::write(&p, body).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    git(&primary, &["init", "-q", "-b", "main"]);
    std::fs::write(primary.join("README"), "x\n").unwrap();
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
    let mut c = Command::new(env!("CARGO_BIN_EXE_compass"));
    c.args(args)
        .env("HOME", &fx.home)
        .env("PATH", path)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .current_dir(cwd)
        .stdin(Stdio::null());
    for k in GIT_ENV {
        c.env_remove(k);
    }
    for (k, v) in envs {
        c.env(k, v);
    }
    let out = c.output().expect("compass runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn charter_write(fx: &Fixture, cwd: &Path, envs: &[(&str, &Path)]) -> (i32, String, String) {
    run(fx, cwd, &["charter", "--write", CHARTER_JSON], envs)
}

// ---- 1. GIT_DIR / GIT_WORK_TREE must not move the boundary ----------------

#[test]
fn git_dir_of_linked_worktree_does_not_unlock_a_primary_cwd() {
    let fx = fixture();
    let gitdir = fx.primary.join(".git/worktrees/linked");
    assert!(gitdir.is_dir(), "fixture precondition");
    let (code, out, err) = charter_write(&fx, &fx.primary, &[("GIT_DIR", &gitdir)]);
    assert_ne!(
        code, 0,
        "GIT_DIR must not unlock a primary cwd; out={out:?} err={err:?}"
    );
    assert!(
        !fx.primary.join(".compass").exists(),
        "nothing may be written into the primary tree; out={out:?} err={err:?}"
    );
}

#[test]
fn git_dir_and_work_tree_of_linked_worktree_do_not_unlock_a_primary_cwd() {
    let fx = fixture();
    let gitdir = fx.primary.join(".git/worktrees/linked");
    let (code, out, err) = charter_write(
        &fx,
        &fx.primary,
        &[("GIT_DIR", &gitdir), ("GIT_WORK_TREE", &fx.linked)],
    );
    assert_ne!(
        code, 0,
        "GIT_DIR+GIT_WORK_TREE must not unlock a primary cwd; out={out:?} err={err:?}"
    );
    assert!(
        !fx.primary.join(".compass").exists(),
        "nothing may be written into the primary tree; out={out:?} err={err:?}"
    );
}

#[test]
fn git_dir_of_a_separate_bare_repo_does_not_unlock_a_primary_cwd() {
    let fx = fixture();
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
    let (code, out, err) = charter_write(&fx, &fx.primary, &[("GIT_DIR", &bare)]);
    assert_ne!(
        code, 0,
        "bare GIT_DIR must not unlock a primary cwd; out={out:?} err={err:?}"
    );
    assert!(
        !fx.primary.join(".compass").exists(),
        "nothing may be written into the primary tree; out={out:?} err={err:?}"
    );
}

/// Control: the PATH decides. A linked-worktree cwd stays writable even when
/// GIT_DIR names the primary's git dir.
#[test]
fn git_dir_of_primary_does_not_lock_a_linked_worktree_cwd() {
    let fx = fixture();
    let gitdir = fx.primary.join(".git");
    let (code, out, err) = charter_write(&fx, &fx.linked, &[("GIT_DIR", &gitdir)]);
    assert_eq!(
        code, 0,
        "a linked-worktree cwd must stay writable regardless of GIT_DIR; out={out:?} err={err:?}"
    );
    assert!(fx.linked.join(".compass/charter.md").exists());
    assert!(!fx.primary.join(".compass").exists());
}

// ---- 3. route ---------------------------------------------------------------

fn route_file(fx: &Fixture) -> PathBuf {
    let p = fx.base.join("decomposition.json");
    std::fs::write(&p, DECOMPOSITION).unwrap();
    p
}

#[test]
fn route_from_primary_checkout_is_refused_and_writes_nothing_under_claude() {
    let fx = fixture();
    let f = route_file(&fx);
    let (code, out, err) = run(
        &fx,
        &fx.primary,
        &["route", "--file", f.to_str().unwrap()],
        &[],
    );
    assert_ne!(
        code, 0,
        "route from primary must be refused; out={out:?} err={err:?}"
    );
    assert!(
        !fx.primary.join(".claude").exists(),
        "refused route must not write .claude/progress.md; out={out:?} err={err:?}"
    );
}

#[test]
fn route_from_linked_worktree_succeeds_and_parks_there_only() {
    let fx = fixture();
    let f = route_file(&fx);
    let (code, out, err) = run(
        &fx,
        &fx.linked,
        &["route", "--file", f.to_str().unwrap()],
        &[],
    );
    assert_eq!(code, 0, "out={out:?} err={err:?}");
    let body = std::fs::read_to_string(fx.linked.join(".claude/progress.md"))
        .expect("progress.md written in the worktree");
    assert!(body.contains("parked-big-move"), "{body}");
    assert!(!fx.primary.join(".claude").exists());
}

// ---- 4. cwd inside <primary>/.git ------------------------------------------

#[test]
fn charter_write_with_cwd_inside_dot_git_is_refused_and_writes_nothing() {
    let fx = fixture();
    let dot_git = fx.primary.join(".git");
    let (code, out, err) = charter_write(&fx, &dot_git, &[]);
    assert_ne!(
        code, 0,
        "a cwd inside <primary>/.git is not a free pass; out={out:?} err={err:?}"
    );
    assert!(
        !dot_git.join(".compass").exists(),
        "nothing may be written under .git; out={out:?} err={err:?}"
    );
    assert!(!fx.primary.join(".compass").exists());
}

// ---- 5. Undetermined, end to end -------------------------------------------

#[test]
fn charter_write_where_dot_git_points_at_a_missing_gitdir_is_refused() {
    let fx = fixture();
    let broken = fx.base.join("broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(
        broken.join(".git"),
        format!("gitdir: {}\n", fx.base.join("no-such-gitdir").display()),
    )
    .unwrap();
    let (code, out, err) = charter_write(&fx, &broken, &[]);
    assert_ne!(
        code, 0,
        "an undetermined boundary must be refused; out={out:?} err={err:?}"
    );
    assert!(
        err.contains("could not determine"),
        "refusal must be for the undetermined boundary, not an unrelated error; stderr={err:?}"
    );
    assert!(
        !broken.join(".compass").exists(),
        "nothing may be written when the boundary is undetermined; out={out:?} err={err:?}"
    );
}
