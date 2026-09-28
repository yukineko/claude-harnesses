//! `backlog merge-driver <base> <ours> <theirs>` (backlog 867dde4b).
//!
//! Contract (human-ruled): git merge-driver convention (%O %A %B), result is
//! written into the <ours> path; exit 0 = clean, non-zero = conflict. Merge is
//! at task-id granularity (union of ids, base-relative deletions honoured).
//! The same id edited differently on both sides is a CONFLICT (never a silently
//! chosen winner). Any unparseable input/output or dropped id is non-zero.
//!
//! Hermetic: a temp git repo per test; the driver is wired with
//! `git config merge.backlog.driver` directly. Written by the test author, not
//! the implementer (CLAUDE.md section 2(a)); RED because the subcommand does
//! not exist yet.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_backlog");

fn unique_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "backlog-mergedrv-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn task(id: &str, title: &str) -> String {
    format!(
        "[[task]]\nid = \"{id}\"\ntitle = \"{title}\"\nproject = \"/x/proj\"\nproject_unresolved = false\ntags = [\"p0\"]\ntouched_files = []\nstatus = \"pending\"\nnotes = \"n-{id}\"\n\n"
    )
}

fn file(tasks: &[(&str, &str)]) -> String {
    tasks.iter().map(|(i, t)| task(i, t)).collect()
}

fn git(repo: &Path, args: &[&str]) -> (i32, String) {
    let o = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git runs");
    let s = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    (o.status.code().unwrap_or(-1), s)
}

fn must(repo: &Path, args: &[&str]) {
    let (c, s) = git(repo, args);
    assert_eq!(c, 0, "git {args:?} failed: {s}");
}

/// Repo on `main` holding `base` at .backlog/tasks.toml.
fn setup(tag: &str, base: &str, with_driver: bool) -> PathBuf {
    let repo = unique_dir(tag);
    must(&repo, &["init", "-q", "-b", "main"]);
    must(&repo, &["config", "user.email", "t@example.com"]);
    must(&repo, &["config", "user.name", "t"]);
    must(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::create_dir_all(repo.join(".backlog")).unwrap();
    std::fs::write(
        repo.join(".gitattributes"),
        ".backlog/tasks.toml merge=backlog\n.backlog/tasks.done.toml merge=backlog\n",
    )
    .unwrap();
    if with_driver {
        must(
            &repo,
            &[
                "config",
                "merge.backlog.driver",
                &format!("{BIN} merge-driver %O %A %B"),
            ],
        );
    }
    std::fs::write(repo.join(".backlog/tasks.toml"), base).unwrap();
    must(&repo, &["add", "-A"]);
    must(&repo, &["commit", "-q", "-m", "base"]);
    repo
}

fn commit_on(repo: &Path, branch: &str, content: &str) {
    must(repo, &["checkout", "-q", "-b", branch, "main"]);
    std::fs::write(repo.join(".backlog/tasks.toml"), content).unwrap();
    must(repo, &["commit", "-q", "-am", branch]);
}

fn ids_of(text: &str) -> Vec<String> {
    let v: toml::Value = toml::from_str(text).expect("merged file must parse as TOML");
    let mut ids: Vec<String> = v
        .get("task")
        .and_then(|t| t.as_array())
        .map(|a| {
            a.iter()
                .map(|t| t["id"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}

fn title_of(text: &str, id: &str) -> String {
    let v: toml::Value = toml::from_str(text).unwrap();
    v["task"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"].as_str() == Some(id))
        .unwrap()["title"]
        .as_str()
        .unwrap()
        .to_string()
}

// (0) control: harness sanity, independent of the driver.
#[test]
fn control_repo_setup_parses() {
    let base = file(&[("aaaaaaaa", "A"), ("bbbbbbbb", "B")]);
    let repo = setup("ctl", &base, false);
    let text = std::fs::read_to_string(repo.join(".backlog/tasks.toml")).unwrap();
    assert_eq!(ids_of(&text), vec!["aaaaaaaa", "bbbbbbbb"]);
}

// (1) both sides add different tasks -> clean, union.
#[test]
fn both_add_different_tasks_merges_clean_with_union() {
    let base = file(&[("aaaaaaaa", "A")]);
    let repo = setup("add", &base, true);
    commit_on(
        &repo,
        "one",
        &file(&[("aaaaaaaa", "A"), ("11111111", "one")]),
    );
    must(&repo, &["checkout", "-q", "main"]);
    commit_on(
        &repo,
        "two",
        &file(&[("aaaaaaaa", "A"), ("22222222", "two")]),
    );
    let (rc, out) = git(&repo, &["merge", "--no-edit", "one"]);
    assert_eq!(rc, 0, "merge one into two must be clean: {out}");
    let text = std::fs::read_to_string(repo.join(".backlog/tasks.toml")).unwrap();
    assert_eq!(ids_of(&text), vec!["11111111", "22222222", "aaaaaaaa"]);
    assert!(!text.contains("<<<<<<<"), "no conflict markers: {text}");
}

// (2) one edits X, other adds Y -> both present, X edited.
#[test]
fn edit_and_add_both_survive() {
    let base = file(&[("aaaaaaaa", "A"), ("bbbbbbbb", "B")]);
    let repo = setup("editadd", &base, true);
    commit_on(
        &repo,
        "edit",
        &file(&[("aaaaaaaa", "A-EDITED"), ("bbbbbbbb", "B")]),
    );
    must(&repo, &["checkout", "-q", "main"]);
    commit_on(
        &repo,
        "add",
        &file(&[("aaaaaaaa", "A"), ("bbbbbbbb", "B"), ("cccccccc", "C")]),
    );
    let (rc, out) = git(&repo, &["merge", "--no-edit", "edit"]);
    assert_eq!(rc, 0, "must merge clean: {out}");
    let text = std::fs::read_to_string(repo.join(".backlog/tasks.toml")).unwrap();
    assert_eq!(ids_of(&text), vec!["aaaaaaaa", "bbbbbbbb", "cccccccc"]);
    assert_eq!(title_of(&text, "aaaaaaaa"), "A-EDITED");
}

// (3) same id edited differently -> conflict, no silent winner.
#[test]
fn same_task_edited_differently_conflicts() {
    let base = file(&[("aaaaaaaa", "A")]);
    let repo = setup("conflict", &base, true);
    commit_on(&repo, "left", &file(&[("aaaaaaaa", "LEFT")]));
    must(&repo, &["checkout", "-q", "main"]);
    commit_on(&repo, "right", &file(&[("aaaaaaaa", "RIGHT")]));
    let (rc, out) = git(&repo, &["merge", "--no-edit", "left"]);
    assert_ne!(rc, 0, "divergent edit of one id must conflict: {out}");
    let (_, status) = git(&repo, &["status", "--porcelain"]);
    assert!(
        status.contains("UU .backlog/tasks.toml"),
        "file must be left unmerged, got: {status}"
    );
}

// Deletion relative to base is honoured (removed on one side, untouched on other).
#[test]
fn deletion_on_one_side_stays_deleted() {
    let base = file(&[("aaaaaaaa", "A"), ("bbbbbbbb", "B")]);
    let repo = setup("del", &base, true);
    commit_on(&repo, "del", &file(&[("aaaaaaaa", "A")]));
    must(&repo, &["checkout", "-q", "main"]);
    commit_on(
        &repo,
        "add",
        &file(&[("aaaaaaaa", "A"), ("bbbbbbbb", "B"), ("dddddddd", "D")]),
    );
    let (rc, out) = git(&repo, &["merge", "--no-edit", "del"]);
    assert_eq!(rc, 0, "must merge clean: {out}");
    let text = std::fs::read_to_string(repo.join(".backlog/tasks.toml")).unwrap();
    assert_eq!(ids_of(&text), vec!["aaaaaaaa", "dddddddd"]);
}

// (4) direct invocation, unparseable theirs -> non-zero, ours untouched.
#[test]
fn unparseable_theirs_fails_and_leaves_ours_untouched() {
    let dir = unique_dir("direct");
    let base = dir.join("base.toml");
    let ours = dir.join("ours.toml");
    let theirs = dir.join("theirs.toml");
    let ours_t = file(&[("aaaaaaaa", "A"), ("11111111", "one")]);
    std::fs::write(&base, file(&[("aaaaaaaa", "A")])).unwrap();
    std::fs::write(&ours, &ours_t).unwrap();
    std::fs::write(&theirs, "[[task]\nid = = broken").unwrap();
    let o = Command::new(BIN)
        .arg("merge-driver")
        .arg(&base)
        .arg(&ours)
        .arg(&theirs)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        !o.status.success(),
        "unparseable theirs must be non-zero; stderr: {err}"
    );
    // rc != 0 alone is also what clap's unknown-subcommand gives; require that
    // the subcommand exists so this cannot pass vacuously.
    assert!(
        !err.contains("unrecognized subcommand"),
        "merge-driver subcommand must exist: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(&ours).unwrap(),
        ours_t,
        "ours must not be replaced by a partial result"
    );
}

// Direct anti-vacuity: the good path exits 0 and writes the union into ours.
#[test]
fn direct_clean_merge_writes_union_into_ours() {
    let dir = unique_dir("directok");
    let (base, ours, theirs) = (dir.join("b"), dir.join("o"), dir.join("t"));
    std::fs::write(&base, file(&[("aaaaaaaa", "A")])).unwrap();
    std::fs::write(&ours, file(&[("aaaaaaaa", "A"), ("11111111", "one")])).unwrap();
    std::fs::write(&theirs, file(&[("aaaaaaaa", "A"), ("22222222", "two")])).unwrap();
    let o = Command::new(BIN)
        .arg("merge-driver")
        .arg(&base)
        .arg(&ours)
        .arg(&theirs)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let text = std::fs::read_to_string(&ours).unwrap();
    assert_eq!(ids_of(&text), vec!["11111111", "22222222", "aaaaaaaa"]);
}
