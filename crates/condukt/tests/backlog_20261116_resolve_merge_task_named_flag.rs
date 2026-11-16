// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 3a45a57d, user ruling 2026-11-16 (resolve-merge, option B): when a
//! merge is held as a merge-conflict entry and later integrated with
//! `condukt worktree resolve-merge --id <conflict_id>`, the hold entry records
//! whether the ORIGINAL merge named a task, and resolve-merge applies the
//! `Condukt-Task` trailer guard ONLY when it did. A legacy entry without that
//! flag (or an unreadable flag) applies the guard (restrictive side).
//!
//! Readings of today's code (`resolve_merge` passes `cli_task = None`, so the
//! guard identity comes from the create-time `branch.<b>.conduktTask` record
//! alone):
//!  (A) DEFECT (case 1): a merge that named no task was held, a trailerless
//!      commit then landed on the branch, and resolve-merge refuses with
//!      `merge-task-trailer-missing` although the original merge never had a
//!      task association (the 2026-11-15 ruling says no guard there).
//!  (B) INTENDED (cases 2, 3): a task-named hold, and a legacy entry, still
//!      refuse.
//!
//! Sequence (all through the real `condukt` binary, sandboxed HOME): create
//! the branch via `worktree create --run runA --topic side` (records
//! `conduktTask`); a TRAILERED commit that conflicts with a commit on main (so
//! the merge reaches the conflict trial and is recorded; the trailer gate runs
//! before the trial, so a trailerless conflicting commit could never be held);
//! `worktree merge` (case 1: no flags; case 2: `--run runA --task side`);
//! a human `theirs` resolution appended to the overwatch resolutions ledger
//! (file append of the serde form of `MergeConflictResolution`: overwatch's
//! CLI is not a binary of this crate); a trailerless commit lands on the
//! branch while held; `worktree resolve-merge --id`.
//!
//! Case 3 (legacy) strips from the recorded entry every JSON key that is not
//! one of the ten pre-flag fields, so it needs no knowledge of the new flag's
//! name and compiles against both old and new code.
//!
//! DOES NOT PROVE: that an UNREADABLE flag value applies the guard (not
//! driven); the `ours`/`manual` resolution choices (only `theirs`); the
//! overwatch CLI path of recording a resolution (a file append is equivalent
//! only as far as the ledger's serde form); concurrent sessions; that the
//! branch observed live on 2026-11-15 had a recorded identity.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn git(dir: &Path, args: &[&str]) -> Output {
    harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("spawn git")
}

fn run_git(dir: &Path, args: &[&str]) -> String {
    let out = git(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn condukt(repo: &Path, home: &Path, args: &[&str]) -> Output {
    harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("CLAUDE_CODE_SESSION_ID", "sess-resolve-merge-flag")
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("CONDUKT_DISABLE")
        .output()
        .expect("spawn condukt")
}

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
    wt: PathBuf,
    branch: String,
}

/// Repo on `main` with `shared.txt`; a worktree branch (CLI-created, so the
/// task identity `runA/side` is recorded) whose trailered commit edits
/// `shared.txt`, and a conflicting edit of `shared.txt` on `main`.
fn setup() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    run_git(&repo, &["init", "-q", "-b", "main"]);
    run_git(&repo, &["config", "user.email", "t@t.t"]);
    run_git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("shared.txt"), "line1\nline2\nline3\n").unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let out = condukt(
        &repo,
        &home,
        &[
            "worktree", "create", "--run", "runA", "--topic", "side", "--branch", "side",
        ],
    );
    assert!(out.status.success(), "worktree create: {out:?}");
    let wt = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
    let branch = run_git(&wt, &["branch", "--show-current"]);
    std::fs::write(wt.join("shared.txt"), "line1\nbranch version\nline3\n").unwrap();
    run_git(&wt, &["add", "shared.txt"]);
    run_git(
        &wt,
        &[
            "commit",
            "-q",
            "-m",
            "branch edit",
            "--trailer",
            "Condukt-Task: runA/side",
        ],
    );
    std::fs::write(repo.join("shared.txt"), "line1\nmain version\nline3\n").unwrap();
    run_git(&repo, &["add", "shared.txt"]);
    run_git(&repo, &["commit", "-q", "-m", "main edit"]);
    Fixture {
        _tmp: tmp,
        repo,
        home,
        wt,
        branch,
    }
}

/// `$HOME/.overwatch/<project key>/overwatch/` (the project key is derived by
/// overwatch; the sandboxed HOME holds exactly one project).
fn store_dir(f: &Fixture) -> PathBuf {
    let root = f.home.join(".overwatch");
    let mut found: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap_or_else(|e| panic!("no overwatch store at {}: {e}", root.display()))
        .map(|e| e.unwrap().path().join("overwatch"))
        .filter(|p| p.join("merge_conflicts.jsonl").exists())
        .collect();
    assert_eq!(found.len(), 1, "expected one overwatch store: {found:?}");
    found.remove(0)
}

fn run_merge(f: &Fixture, extra: &[&str]) {
    let mut args = vec!["worktree", "merge", "--branch", f.branch.as_str()];
    args.extend_from_slice(extra);
    let out = condukt(&f.repo, &f.home, &args);
    eprintln!(
        "merge {extra:?} exit={:?}\nstdout={}\nstderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "fixture precondition: the held merge must be recorded (exit 0); stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Conflict id of the (single) recorded merge-conflict entry.
fn recorded_conflict_id(f: &Fixture) -> String {
    let text = std::fs::read_to_string(store_dir(f).join("merge_conflicts.jsonl")).unwrap();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "expected one conflict entry: {text:?}");
    let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    v["conflict_id"].as_str().unwrap().to_string()
}

/// Human resolution `theirs` (serde form of `MergeConflictResolution`).
fn record_theirs(f: &Fixture, conflict_id: &str) {
    use std::io::Write;
    let line = serde_json::json!({
        "conflict_id": conflict_id,
        "choice": "theirs",
        "decided_by": "human",
        "note": null,
        "ts": 1_700_000_000_i64,
    });
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(store_dir(f).join("merge_conflict_resolutions.jsonl"))
        .unwrap();
    writeln!(file, "{line}").unwrap();
}

/// Land a trailerless commit on the branch while the merge is held.
fn land_trailerless(f: &Fixture) -> String {
    std::fs::write(f.wt.join("extra.txt"), "extra\n").unwrap();
    run_git(&f.wt, &["add", "extra.txt"]);
    run_git(&f.wt, &["commit", "-q", "-m", "foreign trailerless commit"]);
    run_git(&f.wt, &["rev-parse", "HEAD"])
}

fn resolve(f: &Fixture, id: &str) -> (Output, String) {
    let out = condukt(&f.repo, &f.home, &["worktree", "resolve-merge", "--id", id]);
    eprintln!(
        "resolve-merge exit={:?}\nstdout={}\nstderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    (out, stderr)
}

fn assert_trailer_refusal(f: &Fixture, out: &Output, stderr: &str, main_before: &str) {
    assert!(!out.status.success(), "resolve-merge must refuse");
    assert!(
        stderr.contains("merge-task-trailer-missing"),
        "refusal must be the trailer guard; stderr={stderr:?}"
    );
    assert_eq!(
        run_git(&f.repo, &["rev-parse", "main"]),
        main_before,
        "a refused resolve-merge must leave main untouched"
    );
}

/// RED (the defect): the original merge named NO task, so resolve-merge must
/// not apply the trailer guard to a trailerless commit added while held.
#[test]
fn resolve_merge_of_no_task_hold_does_not_apply_trailer_guard() {
    let f = setup();
    run_merge(&f, &[]);
    let id = recorded_conflict_id(&f);
    record_theirs(&f, &id);
    let tip = land_trailerless(&f);
    let main_before = run_git(&f.repo, &["rev-parse", "main"]);
    let (out, stderr) = resolve(&f, &id);
    assert!(
        !stderr.contains("merge-task-trailer-missing"),
        "no-task hold: the trailer guard must not refuse resolve-merge; stderr={stderr:?}"
    );
    assert!(
        out.status.success(),
        "resolve-merge of a no-task hold must succeed; stderr={stderr:?}"
    );
    assert_ne!(run_git(&f.repo, &["rev-parse", "main"]), main_before);
    assert!(
        git(&f.repo, &["merge-base", "--is-ancestor", &tip, "main"])
            .status
            .success(),
        "default branch must contain the branch tip {tip}"
    );
}

/// Control: the original merge named the task => the guard still applies.
#[test]
fn control_resolve_merge_of_task_named_hold_refuses_trailerless_commit() {
    let f = setup();
    run_merge(&f, &["--run", "runA", "--task", "side"]);
    let id = recorded_conflict_id(&f);
    record_theirs(&f, &id);
    land_trailerless(&f);
    let main_before = run_git(&f.repo, &["rev-parse", "main"]);
    let (out, stderr) = resolve(&f, &id);
    assert_trailer_refusal(&f, &out, &stderr, &main_before);
}

/// Control (legacy): an entry carrying none of the post-flag keys applies the
/// guard (restrictive default).
#[test]
fn control_resolve_merge_of_legacy_entry_refuses_trailerless_commit() {
    const LEGACY_KEYS: [&str; 10] = [
        "conflict_id",
        "origin",
        "run_id",
        "branch",
        "default_branch",
        "base_ref",
        "conflicted_files",
        "diff_ours",
        "diff_theirs",
        "ts",
    ];
    let f = setup();
    run_merge(&f, &[]);
    let id = recorded_conflict_id(&f);
    let path = store_dir(&f).join("merge_conflicts.jsonl");
    let text = std::fs::read_to_string(&path).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    v.as_object_mut()
        .unwrap()
        .retain(|k, _| LEGACY_KEYS.contains(&k.as_str()));
    std::fs::write(&path, format!("{v}\n")).unwrap();
    record_theirs(&f, &id);
    land_trailerless(&f);
    let main_before = run_git(&f.repo, &["rev-parse", "main"]);
    let (out, stderr) = resolve(&f, &id);
    assert_trailer_refusal(&f, &out, &stderr, &main_before);
}
