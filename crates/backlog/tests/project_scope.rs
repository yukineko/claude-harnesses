//! The store IS the scope: a repo store is not filtered again by project label.
//!
//! Background, measured 2026-08-20 at measurement point `bb046648`. The queue
//! resolves per repo (`config::locate` → `<root>/.backlog/tasks.toml`),
//! and that file is tracked and shared through git. But reads applied a SECOND
//! filter on top of it: the `project` field, an absolute path recorded by
//! whichever machine wrote the task. In this repo's own store that split one
//! project into three:
//!
//!     258  /Users/yuki/src/harness                       (the mac checkout)
//!      66  /mnt/c/Users/hiroyuki_nakayama/src/harness     (this checkout)
//!       5  C:/Users/hiroyuki_nakayama/src/harness
//!
//! `backlog list` from WSL showed 66 and silently dropped the other 263 — all
//! of them tasks of the very repo whose file was being read. That is the same
//! collapse 5ba13c3e closed one level up (an answer about the wrong scope
//! rendered exactly like an answer about an empty queue), so the fix is the
//! same shape: make the scope the STORE, and let `--project` be an ASSERTION
//! about which store you meant rather than a row filter.
//!
//! The second half is the write side. `locate` used to fall back to the
//! cross-project `~/.backlog` when no repo root was found above the cwd, so a
//! process running in a tempdir wrote its tasks into the operator's real queue
//! (observed: specforge's spec-ratify fixtures landed there under
//! `project="/tmp/.tmpsYSvwG"`, backlog 7d8ab7fe). "I have no project store"
//! must refuse, not pick a shared one.
//!
//! A PINNED `store_dir` keeps the old behaviour on both counts, and the two
//! controls at the bottom of this file are what stop the change from being a
//! blanket removal of the filter: a pinned store really can hold several
//! projects, so there the label is the only thing that separates them.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn unique(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-scope-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_in(args: &[&str], home: &Path, cwd: &Path) -> (i32, String, String) {
    let bin = env!("CARGO_BIN_EXE_backlog");
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .env("HOME", home)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("binary spawns");
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(b"");
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// A real `git init`ed repo — not a hand-made `.git` dir, because project
/// IDENTITY resolution reads the `.git` layout for real (`main_worktree_of`).
fn init_repo(tag: &str) -> PathBuf {
    assert!(
        git_available(),
        "git is unavailable, so this test cannot observe anything; failing rather \
         than reporting green"
    );
    let root = unique(tag);
    let ok = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .status()
        .expect("git init runs")
        .success();
    assert!(ok, "git init failed in {}", root.display());
    root
}

/// Write the repo's store directly. Deliberately NOT via `backlog add`: the
/// point is what a store written by ANOTHER checkout looks like when read
/// here, and going through `add` would bake in whatever label this checkout
/// happens to record.
fn write_store(root: &Path, body: &str) {
    let dir = root.join(".backlog");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("tasks.toml"), body).unwrap();
}

fn task(id: &str, title: &str, project: &str) -> String {
    format!(
        "[[task]]\nid = \"{id}\"\ntitle = \"{title}\"\nproject = \"{project}\"\n\
         tags = [\"p1\"]\nstatus = \"pending\"\nnotes = \"\"\n\
         created_at = 1000\nupdated_at = 1000\nweight = 0.0\n\n"
    )
}

/// The nearest ancestor holding `.git`, mirroring `config::repo_root`.
fn has_repo_above(start: &Path) -> bool {
    let mut dir = start.to_path_buf();
    loop {
        if dir.join(".git").exists() {
            return true;
        }
        if !dir.pop() {
            return false;
        }
    }
}

const FOREIGN: &str = "/Users/some-other-machine/src/thing";

// ---- the store is the scope -------------------------------------------------

/// The defect this file exists for: a task in THIS repo's store, recorded by
/// another checkout of the same repo, must be listed here.
#[test]
fn a_task_recorded_under_another_checkouts_label_is_listed() {
    let home = unique("home-a");
    let root = init_repo("repo-a");
    write_store(
        &root,
        &task("aaaaaaaa", "written on the other machine", FOREIGN),
    );

    let (rc, out, err) = run_in(&["list", "--status", "pending"], &home, &root);
    assert_eq!(rc, 0, "out={out}\nerr={err}");
    assert!(
        out.contains("aaaaaaaa"),
        "a task in this repo's own store was dropped because its project label \
         names another checkout of the same repo.\nout={out}\nerr={err}"
    );
}

/// `--project <this repo>` is an assertion that holds, so it must not change
/// the answer. (`/flow` passes `--project "$PWD"` on every call.)
#[test]
fn an_explicit_project_naming_this_repo_changes_nothing() {
    let home = unique("home-b");
    let root = init_repo("repo-b");
    write_store(&root, &task("bbbbbbbb", "still mine", FOREIGN));

    let (rc, out, err) = run_in(
        &[
            "list",
            "--status",
            "pending",
            "--project",
            root.to_str().unwrap(),
        ],
        &home,
        &root,
    );
    assert_eq!(rc, 0, "out={out}\nerr={err}");
    assert!(
        out.contains("bbbbbbbb"),
        "--project naming this very repo filtered out this repo's task.\nout={out}\nerr={err}"
    );
}

/// `--project <a different repo>` cannot be satisfied by this store, and
/// answering it with a filtered (probably empty) listing is the collapse this
/// change is about: the reader cannot tell "nothing matched" from "you asked
/// the wrong store".
#[test]
fn an_explicit_project_naming_a_different_repo_is_refused() {
    let home = unique("home-c");
    let root = init_repo("repo-c");
    let other = unique("repo-c-other");
    write_store(&root, &task("cccccccc", "mine", FOREIGN));

    let (rc, out, err) = run_in(
        &[
            "list",
            "--status",
            "pending",
            "--project",
            other.to_str().unwrap(),
        ],
        &home,
        &root,
    );
    assert_ne!(
        rc, 0,
        "asking a repo store for another repo's tasks was answered instead of \
         refused.\nout={out}\nerr={err}"
    );
    assert!(
        out.is_empty(),
        "a refused query must print nothing on stdout.\nout={out}"
    );
    assert!(
        err.contains(other.to_str().unwrap()),
        "the diagnostic must name the project that was asked for.\nerr={err}"
    );
}

/// `next` is the question a driver asks, so it has to agree with `list` about
/// scope — a task `list` shows and `next` withholds is the worse half of the
/// same bug.
#[test]
fn next_hands_out_a_task_recorded_under_another_checkouts_label() {
    let home = unique("home-d");
    let root = init_repo("repo-d");
    write_store(&root, &task("dddddddd", "pick me", FOREIGN));

    let (rc, out, err) = run_in(&["next"], &home, &root);
    assert_eq!(rc, 0, "out={out}\nerr={err}");
    assert!(
        out.contains("dddddddd"),
        "next reported no work while this repo's store held a pending task.\nout={out}\nerr={err}"
    );
}

// ---- no project store means refuse, not "use the shared one" ---------------

#[test]
fn add_outside_any_repo_refuses_instead_of_writing_the_shared_store() {
    let home = unique("home-e");
    let orphan = unique("orphan-e");
    assert!(
        !has_repo_above(&orphan),
        "{} is inside a git repo, so this machine cannot host this test; failing \
         rather than passing vacuously",
        orphan.display()
    );

    let (rc, out, err) = run_in(
        &[
            "add",
            "--title",
            "leaked fixture",
            "--project",
            "/tmp/whatever",
        ],
        &home,
        &orphan,
    );
    assert_ne!(
        rc, 0,
        "a cwd with no repo above it wrote into the cross-project store.\nout={out}\nerr={err}"
    );
    assert!(
        !home.join(".backlog").join("tasks.toml").exists(),
        "the refusal still created {}",
        home.join(".backlog").join("tasks.toml").display()
    );
}

#[test]
fn list_outside_any_repo_refuses_instead_of_reading_the_shared_store() {
    let home = unique("home-f");
    let orphan = unique("orphan-f");
    assert!(
        !has_repo_above(&orphan),
        "{} is inside a git repo; failing rather than passing vacuously",
        orphan.display()
    );
    // A real queue in the shared store, so an empty answer cannot be mistaken
    // for "there was nothing there anyway".
    std::fs::create_dir_all(home.join(".backlog")).unwrap();
    std::fs::write(
        home.join(".backlog").join("tasks.toml"),
        task("ffffffff", "someone elses project", FOREIGN),
    )
    .unwrap();

    let (rc, out, err) = run_in(&["list", "--status", "pending"], &home, &orphan);
    assert_ne!(
        rc, 0,
        "a cwd with no repo above it read the cross-project store.\nout={out}\nerr={err}"
    );
    assert!(
        !out.contains("ffffffff"),
        "another project's task was listed.\nout={out}"
    );
}

// ---- controls: the pinned store keeps BOTH old behaviours ------------------
//
// These two are the anti-vacuity pair. They pass before the change and must
// keep passing after it: if the fix were "delete the project filter" or
// "delete the legacy fallback" outright, one of them breaks.

/// A pinned store is explicitly cross-project, so there the label is the only
/// thing separating two projects and the row filter must stay.
#[test]
fn a_pinned_store_dir_still_scopes_by_project() {
    let home = unique("home-g");
    let pinned = unique("pinned-g");
    let root = init_repo("repo-g");
    std::fs::create_dir_all(home.join(".backlog")).unwrap();
    std::fs::write(
        home.join(".backlog").join("config.toml"),
        format!("store_dir = \"{}\"\n", pinned.display()),
    )
    .unwrap();
    let mut body = task("11111111", "belongs here", root.to_str().unwrap());
    body.push_str(&task("22222222", "belongs elsewhere", FOREIGN));
    std::fs::write(pinned.join("tasks.toml"), body).unwrap();

    let (rc, out, err) = run_in(&["list", "--status", "pending"], &home, &root);
    assert_eq!(rc, 0, "out={out}\nerr={err}");
    assert!(out.contains("11111111"), "out={out}\nerr={err}");
    assert!(
        !out.contains("22222222"),
        "a pinned store is cross-project; another project's task must not be \
         listed here.\nout={out}"
    );
}

/// The escape hatch named by the refusal message has to actually work: with
/// `store_dir` pinned, a cwd outside any repo is a supported configuration.
#[test]
fn a_pinned_store_dir_still_works_outside_any_repo() {
    let home = unique("home-h");
    let pinned = unique("pinned-h");
    let orphan = unique("orphan-h");
    assert!(
        !has_repo_above(&orphan),
        "{} is inside a git repo; failing rather than passing vacuously",
        orphan.display()
    );
    std::fs::create_dir_all(home.join(".backlog")).unwrap();
    std::fs::write(
        home.join(".backlog").join("config.toml"),
        format!("store_dir = \"{}\"\n", pinned.display()),
    )
    .unwrap();
    std::fs::write(
        pinned.join("tasks.toml"),
        task(
            "33333333",
            "pinned and outside a repo",
            orphan.to_str().unwrap(),
        ),
    )
    .unwrap();

    let (rc, out, err) = run_in(&["list", "--status", "pending"], &home, &orphan);
    assert_eq!(
        rc, 0,
        "a pinned store_dir must keep working outside any repo.\nout={out}\nerr={err}"
    );
    assert!(out.contains("33333333"), "out={out}\nerr={err}");
}
