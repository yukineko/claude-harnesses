// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: a written path POSITIVELY outside every git repo makes
//! `session_roots::resolve` Undetermined, so donegate and tdd block every Stop.
//!
//! Observed (2026-10-26): a session that once wrote
//! `/Users/yuki/.budgetguard/config.toml` got, on every Stop, from both gates:
//! `the session's roots cannot be determined (session roots: the session wrote
//! /Users/yuki/.budgetguard/config.toml but its git root cannot be determined:
//! git rev-parse --show-toplevel failed in /Users/yuki/.budgetguard (exit
//! Some(128)): fatal: not a git repository (or any of the parent directories): .git)`.
//!
//! User ruling (same principle as GitHub #1180, `scripts/deny_ledger.py`
//! `_outside_repo_write_target()`): an ABSOLUTE written path whose directory
//! git cleanly reports as "not a git repository (or any of the parent
//! directories)" (exit 128) is excluded from the roots — that is an
//! observation, not an undetermined answer. Every other failure (a corrupt
//! `.git` file, an unenterable directory, a relative path, git missing) stays
//! Undetermined (fail-closed, CLAUDE.md section 3).
//!
//! RED test: `outside_repo_write_is_excluded_and_roots_are_known` (+ the
//! mixed-session twin), `#[ignore]`d until fixed: run with `--include-ignored`.
//! The control tests pass before and must pass after.

use harness_core::session_roots::resolve;
use harness_core::verdict::Determination;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git must be runnable for this test")
}

fn init_repo(dir: &Path) {
    let out = git(dir, &["init", "-q"]);
    assert!(out.status.success(), "git init failed: {out:?}");
}

/// Precondition: `dir` really is outside every repo, observed the same way the
/// ruling defines it. Panics (never skips) when the environment breaks it.
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

/// A one-line JSONL transcript recording a `Write` of each path.
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

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap()
}

/// Layout: `<tmp>/repo` (git repo, the hook cwd), `<tmp>/outside` (no repo),
/// `<tmp>/t` (transcript dir). `<tmp>` itself must be outside every repo.
struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    outside: PathBuf,
    tdir: PathBuf,
    base: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let base = canon(tmp.path());
    assert_positively_outside(&base);
    let repo = base.join("repo");
    let outside = base.join("outside");
    let tdir = base.join("t");
    for d in [&repo, &outside, &tdir] {
        std::fs::create_dir_all(d).unwrap();
    }
    init_repo(&repo);
    assert_positively_outside(&outside);
    Fixture {
        _tmp: tmp,
        repo,
        outside,
        tdir,
        base,
    }
}

// ── RED: must FAIL until the fix lands ───────────────────────────────────────

#[test]
#[ignore = "open defect: outside-repo write makes session roots Undetermined; remove ignore when fixed"]
fn outside_repo_write_is_excluded_and_roots_are_known() {
    let f = fixture();
    let written = f.outside.join("config.toml");
    std::fs::write(&written, "x").unwrap();
    let t = transcript(&f.tdir, &[written.to_str().unwrap()]);
    match resolve(&f.repo, &t) {
        Determination::Known(sr) => {
            assert_eq!(
                sr.roots,
                vec![canon(&f.repo)],
                "the outside-repo write must not add a root"
            );
            assert!(sr.wrote_any, "the session did write a file");
        }
        Determination::Undetermined(why) => panic!(
            "a write positively outside every repo must be excluded, not Undetermined: {}",
            why.as_str()
        ),
    }
}

#[test]
#[ignore = "open defect: outside-repo write makes session roots Undetermined; remove ignore when fixed"]
fn outside_write_plus_linked_repo_write_keeps_the_repo_root() {
    let f = fixture();
    let other = f.base.join("other");
    std::fs::create_dir_all(&other).unwrap();
    init_repo(&other);
    let outside_file = f.outside.join("config.toml");
    let repo_file = other.join("a.rs");
    std::fs::write(&outside_file, "x").unwrap();
    std::fs::write(&repo_file, "x").unwrap();
    let t = transcript(
        &f.tdir,
        &[outside_file.to_str().unwrap(), repo_file.to_str().unwrap()],
    );
    match resolve(&f.repo, &t) {
        Determination::Known(sr) => {
            assert_eq!(sr.roots, vec![canon(&f.repo), canon(&other)]);
        }
        Determination::Undetermined(why) => panic!(
            "outside write must be excluded and the repo write kept: {}",
            why.as_str()
        ),
    }
}

// ── controls: Undetermined now and after the fix ─────────────────────────────

/// A `.git` gitfile pointing nowhere: git also says "not a git repository",
/// but with exit 128 and `: <path>` — a corrupt checkout, not "outside".
#[test]
fn control_corrupt_gitfile_stays_undetermined() {
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
    let file = broken.join("a.rs");
    std::fs::write(&file, "x").unwrap();
    let t = transcript(&f.tdir, &[file.to_str().unwrap()]);
    assert!(
        matches!(resolve(&f.repo, &t), Determination::Undetermined(_)),
        "a corrupt checkout is not 'outside every repo'"
    );
}

/// A directory git cannot enter (mode 000) is a failure, not an observation.
#[cfg(unix)]
#[test]
fn control_unenterable_dir_stays_undetermined() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let locked = f.base.join("locked");
    std::fs::create_dir_all(&locked).unwrap();
    let file = locked.join("a.rs");
    std::fs::write(&file, "x").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let t = transcript(&f.tdir, &[file.to_str().unwrap()]);
    let got = resolve(&f.repo, &t);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        matches!(got, Determination::Undetermined(_)),
        "an unenterable directory is not 'outside every repo': {got:?}"
    );
}

/// A relative written path never qualifies (the ruling requires absolute).
#[test]
fn control_relative_path_stays_undetermined() {
    let f = fixture();
    let t = transcript(&f.tdir, &["config.toml"]);
    assert!(
        matches!(resolve(&f.repo, &t), Determination::Undetermined(_)),
        "a relative written path is not positively outside every repo"
    );
}

/// The written file's directory is gone: still Undetermined.
#[test]
fn control_missing_directory_stays_undetermined() {
    let f = fixture();
    let gone = f.outside.join("gone").join("config.toml");
    let t = transcript(&f.tdir, &[gone.to_str().unwrap()]);
    assert!(matches!(
        resolve(&f.repo, &t),
        Determination::Undetermined(_)
    ));
}
