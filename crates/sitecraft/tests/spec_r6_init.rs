//! Spec `sitecraft-capabilities` R6 — `sitecraft init <dir>` and the `app/`
//! scaffold target (rulings D6, C5).
//!
//! # CLI surface assumed (rulings D6)
//!
//! * `sitecraft init <dir>` (no `--root`; `git` from `PATH`):
//!   - `<dir>` absent or an empty directory → create it if needed, `git init`
//!     it (`<dir>/.git` exists, `git -C <dir> rev-parse --show-toplevel` is
//!     `<dir>`), create the directory `<dir>/sitecraft/`; exit 0.
//!   - `<dir>` non-empty, or a regular file → exit 1, nothing changed.
//!   - `<dir>`'s parent inside another git work tree → exit 1, nothing created.
//!   - no `git` on `PATH`, `git init` non-zero, `git` exceeding 30 s, IO
//!     failure → exit 2.
//! * SKILL.md keeps the invariant text 「空でないディレクトリには scaffold しない」
//!   verbatim, in a paragraph that names `app/` as the scaffold target; its
//!   `sitecraft.toml` example uses `npm --prefix app` (or the argv form
//!   `"npm", "--prefix", "app"`), `build_dir = "app/dist"` and
//!   `web_globs = ["app/src/**", …]`; the Phase 6 section names `app/`.
//!
//! The 30 s git timeout test takes ~30 s (a fake `git` whose `init` sleeps).
//! The fake-git tests leave a marker from inside `git init`, so their exit 2
//! cannot come from argument parsing. IO failure is provoked by a read-only
//! parent of an absent `<dir>`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod spec_common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use spec_common::*;

fn init(p: &Project, dir: &Path, path_env: Option<&str>) -> Out {
    let mut c = p.bare_cmd();
    c.arg("init").arg(dir);
    if let Some(path) = path_env {
        c.env("PATH", path);
    }
    run_deadline(c, None, Duration::from_secs(90))
}

/// Precondition: `dir` is not inside any git work tree.
fn assert_outside_git(dir: &Path) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--is-inside-work-tree"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "precondition: {} must not be inside a git work tree",
        dir.display()
    );
}

fn toplevel(dir: &Path) -> PathBuf {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    assert!(out.status.success(), "{} is not a git repo", dir.display());
    PathBuf::from(String::from_utf8_lossy(&out.stdout).trim())
}

fn listing(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn assert_initialised(dir: &Path) {
    assert!(dir.join(".git").exists(), "{}/.git missing", dir.display());
    assert_eq!(
        toplevel(dir).canonicalize().unwrap(),
        dir.canonicalize().unwrap(),
        "{} must be its own git top level",
        dir.display()
    );
    assert!(
        dir.join("sitecraft").is_dir(),
        "{}/sitecraft/ must be created",
        dir.display()
    );
}

#[test]
fn init_creates_a_missing_directory() {
    let p = Project::new();
    assert_outside_git(&p.base);
    let dir = p.base.join("new-site");
    let o = init(&p, &dir, None);
    expect_code(&o, 0, "init on a missing directory");
    assert_initialised(&dir);
}

#[test]
fn init_accepts_an_empty_directory() {
    let p = Project::new();
    assert_outside_git(&p.base);
    let dir = p.base.join("empty-site");
    std::fs::create_dir_all(&dir).unwrap();
    let o = init(&p, &dir, None);
    expect_code(&o, 0, "init on an empty directory");
    assert_initialised(&dir);
}

#[test]
fn init_refuses_a_non_empty_directory_or_a_file() {
    let p = Project::new();
    assert_outside_git(&p.base);
    let dir = p.base.join("busy-site");
    write(&dir.join("README.md"), "already here\n");
    let o = init(&p, &dir, None);
    expect_code(&o, 1, "init on a non-empty directory");
    assert_eq!(
        listing(&dir),
        vec!["README.md".to_string()],
        "nothing may change"
    );

    let file = p.base.join("a-file");
    write(&file, "x");
    let o = init(&p, &file, None);
    expect_code(&o, 1, "init on a regular file");
    assert_eq!(
        std::fs::read(&file).unwrap(),
        b"x",
        "the file must not change"
    );

    // Positive control: an empty sibling is accepted.
    let ok = p.base.join("ok-site");
    expect_code(&init(&p, &ok, None), 0, "init on a missing sibling");
}

#[test]
fn init_refuses_a_directory_inside_another_work_tree() {
    let p = Project::new_git();
    let dir = p.root.join("nested-site");
    let o = init(&p, &dir, None);
    expect_code(&o, 1, "init inside another git work tree");
    assert!(
        !dir.exists(),
        "nothing may be created inside the outer repo"
    );

    let empty = p.root.join("empty-nested");
    std::fs::create_dir_all(&empty).unwrap();
    let o = init(&p, &empty, None);
    expect_code(&o, 1, "init on an empty dir inside another work tree");
    assert!(!empty.join(".git").exists() && !empty.join("sitecraft").exists());
}

#[test]
fn init_without_git_is_exit_2() {
    let p = Project::new();
    let empty_bin = p.base.join("empty-bin");
    std::fs::create_dir_all(&empty_bin).unwrap();
    let dir = p.base.join("site-nogit");
    let o = init(&p, &dir, Some(&empty_bin.display().to_string()));
    expect_code(&o, 2, "init with no git on PATH");
    // Positive control: with the normal PATH the same target initialises.
    expect_code(&init(&p, &dir, None), 0, "init with git");
}

/// A fake `git`: `init` runs `init_body`; anything else answers like git
/// outside a repository.
fn fake_git(p: &Project, init_body: &str) -> String {
    let bin = p.base.join("fake-git-bin");
    write_exec(
        &bin.join("git"),
        &format!(
            "#!/bin/sh\nfor a in \"$@\"; do\n  if [ \"$a\" = init ]; then\n    {init_body}\n  fi\ndone\necho 'fatal: not a git repository (or any of the parent directories): .git' >&2\nexit 128\n"
        ),
    );
    format!("{}:/usr/bin:/bin", bin.display())
}

#[test]
fn git_init_failure_is_exit_2() {
    let p = Project::new();
    let marker = p.marker("git-init-ran");
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    let path = fake_git(
        &p,
        &format!(
            ": > '{}'; echo 'fatal: cannot init' >&2; exit 1",
            marker.display()
        ),
    );
    let dir = p.base.join("site-initfail");
    let o = init(&p, &dir, Some(&path));
    expect_code(&o, 2, "git init exiting 1");
    assert!(
        marker.exists(),
        "the exit 2 must come from the failed `git init` (it was never run)\n{}",
        o.describe()
    );
}

#[test]
fn git_exceeding_30_seconds_is_exit_2() {
    let p = Project::new();
    let marker = p.marker("git-init-started");
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    let path = fake_git(&p, &format!(": > '{}'; sleep 75; exit 0", marker.display()));
    let dir = p.base.join("site-slowgit");
    let o = init(&p, &dir, Some(&path));
    expect_code(&o, 2, "git init exceeding the 30 s limit");
    assert!(
        marker.exists(),
        "the exit 2 must come from the slow `git init` (it was never run)\n{}",
        o.describe()
    );
    assert!(
        o.elapsed >= Duration::from_secs(25),
        "init must wait for git up to its 30 s limit, gave up after {:?}",
        o.elapsed
    );
    assert!(
        o.elapsed < Duration::from_secs(70),
        "init must give up at 30 s, took {:?}",
        o.elapsed
    );
}

#[test]
fn io_failure_creating_the_directory_is_exit_2() {
    let p = Project::new();
    assert_outside_git(&p.base);
    // `<dir>` is absent and its parent is read-only: creating it fails with
    // an IO error (EACCES), which is undetermined, not a refusal.
    let parent = p.base.join("ro-parent");
    std::fs::create_dir_all(&parent).unwrap();
    chmod(&parent, 0o555);
    let dir = parent.join("site");
    let o = init(&p, &dir, None);
    chmod(&parent, 0o755);
    expect_code(&o, 2, "init whose directory cannot be created (IO failure)");
    assert!(!dir.exists(), "nothing may be created");
    // Positive control: once the parent is writable the same target works.
    let o = init(&p, &dir, None);
    expect_code(&o, 0, "init after the parent became writable");
    assert_initialised(&dir);
}

#[test]
fn skill_keeps_the_invariant_and_names_app_as_the_target() {
    let s = skill_text();
    assert!(
        s.contains("空でないディレクトリには scaffold しない"),
        "the invariant text must stay verbatim"
    );
    assert_paragraph_with(
        &s,
        &["空でないディレクトリには scaffold しない", "app/"],
        "R6: the non-empty rule applies to the app/ scaffold target",
    );
}

#[test]
fn skill_sitecraft_toml_example_uses_app() {
    let s = skill_text();
    assert!(
        s.contains("npm --prefix app") || s.contains("\"npm\", \"--prefix\", \"app\""),
        "the sitecraft.toml example must run `npm --prefix app …`"
    );
    assert!(
        s.contains("build_dir = \"app/dist\""),
        "build_dir = \"app/dist\" missing"
    );
    assert!(
        s.contains("web_globs = [\"app/src/**\""),
        "web_globs = [\"app/src/**\", …] missing"
    );
}

#[test]
fn skill_phase_6_scaffolds_into_app_even_with_a_decision_record() {
    let phase6 = section(&skill_text(), "### Phase 6");
    assert!(!phase6.is_empty(), "SKILL.md has no Phase 6 section");
    assert!(
        phase6.contains("app/"),
        "Phase 6 must scaffold into <root>/app/ (a root holding sitecraft/decisions.toml is not refused)"
    );
}
