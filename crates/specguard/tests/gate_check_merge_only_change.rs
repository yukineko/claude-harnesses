// INTENTIONALLY RED at the commit that adds it. Written by a diagnosing agent
// that did not implement `map build` / `map gate-check`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The pre-push hook (`.githooks/pre-push`) runs, inside a checkout of the
//! pushed sha:
//!
//! ```text
//! specguard map build --baseline <base>
//! specguard map gate-check --base <base> --head <sha>
//! ```
//!
//! The two halves read DIFFERENT views of `<base>..<sha>`:
//!
//! * `map build` -> `SpecMap::sync` -> `git log --name-status <base>..HEAD`
//!   (src/specmap.rs `git_log_name_status`). Without `-m`/`--cc`, `git log`
//!   prints no file list for merge commits, so a change made only in a merge
//!   commit (a conflict resolution / evil merge) is never attributed.
//! * `map gate-check` -> `git diff --name-status <base> <head>`
//!   (src/gatecheck.rs `changed_paths`), a tree diff that DOES see it.
//!
//! So such a path is "referenced by no spec-map entry" (exit 2) no matter what
//! the tracked `.specguard/spec-doc-acks.toml` says — the module doc of
//! src/gatecheck.rs claims "`map sync` attributes every non-excluded path to an
//! entry", and the remedy it prints (run `map sync`, then ack) cannot clear it.
//! Observed live: every `git push origin main` at d41d7652 blocked on
//! crates/blastguard/tests/python_stdin_git_string_fp_20261103.rs, whose only
//! change in origin/main..main was made by merge 82d775c2 (both parents carried
//! blob 0dc54848, the merge result is cb4fc9f1), although that path is acked.
//!
//! Control: the identical content change made in an ordinary commit passes the
//! same pipeline, so the fixture (config, ack file, range) is not what fails.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

const F: &str = "crates/blastguard/tests/merge_only_change.rs";

fn git(repo: &Path, args: &[&str]) -> String {
    let o = harness_core::git_env::scrub(&mut Command::new("git"))
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn write(repo: &Path, rel: &str, body: &str) {
    let p = repo.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

/// Base commit: config, the gate test file F (v1), and a tracked reasoned ack
/// for F. Returns the base sha.
fn fixture(repo: &Path) -> String {
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["config", "user.email", "t@t.t"]);
    git(repo, &["config", "user.name", "t"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    write(
        repo,
        "specguard.toml",
        "[project]\nname = \"Demo\"\nroot = \".\"\n\n[output]\nreport_dir = \"reports\"\nsentinel = \".pending\"\n\n[[area]]\nname = \"crates\"\nglobs = [\"crates/**\"]\ncanon = []\n",
    );
    write(repo, ".gitignore", ".specguard/spec-map.toml\nreports/\n");
    write(repo, F, "#[test]\nfn t() { assert_eq!(1, 1); }\n");
    write(
        repo,
        ".specguard/spec-doc-acks.toml",
        &format!("[[ack]]\npath = \"{F}\"\nreason = \"regression repro; behaviour is specified by the test itself\"\n"),
    );
    write(repo, "README.md", "seed\n");
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "base"]);
    git(repo, &["rev-parse", "HEAD"])
}

const F_V2: &str = "#[test]\n#[ignore]\nfn t() { assert_eq!(1, 1); }\n";

/// base -> side (touches README.md) ; base -> main (touches NOTES.md) ;
/// merge side into main, changing F ONLY in the merge commit.
fn change_f_in_merge_only(repo: &Path) {
    git(repo, &["checkout", "-q", "-b", "side"]);
    write(repo, "README.md", "side\n");
    git(repo, &["commit", "-q", "-am", "side"]);
    git(repo, &["checkout", "-q", "main"]);
    write(repo, "NOTES.md", "main\n");
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "main"]);
    git(repo, &["merge", "-q", "--no-ff", "--no-commit", "side"]);
    write(repo, F, F_V2);
    git(repo, &["add", F]);
    git(
        repo,
        &["commit", "-q", "-m", "merge side; F changed in the merge"],
    );
}

fn change_f_in_plain_commit(repo: &Path) {
    write(repo, F, F_V2);
    git(repo, &["commit", "-q", "-am", "change F"]);
}

fn sg(repo: &Path, home: &Path, args: &[&str]) -> Output {
    harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_specguard")))
        .current_dir(repo)
        .env("HOME", home)
        .env_remove("SPECGUARD_BASELINE_REF")
        .args(["--config", "specguard.toml"])
        .args(args)
        .output()
        .unwrap()
}

/// The hook's two steps, verbatim apart from `--config`.
fn hook_pipeline(repo: &Path, home: &Path, base: &str) -> Output {
    let b = sg(repo, home, &["map", "build", "--baseline", base]);
    assert!(
        b.status.success(),
        "map build must succeed (fixture): {}",
        String::from_utf8_lossy(&b.stderr)
    );
    let head = git(repo, &["rev-parse", "HEAD"]);
    sg(
        repo,
        home,
        &["map", "gate-check", "--base", base, "--head", &head],
    )
}

fn describe(o: &Output) -> String {
    format!(
        "exit={:?}\nstdout={}\nstderr={}",
        o.status.code(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn control_acked_gate_test_changed_in_plain_commit_passes_hook_pipeline() {
    let d = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let base = fixture(d.path());
    change_f_in_plain_commit(d.path());
    let o = hook_pipeline(d.path(), home.path(), &base);
    assert_eq!(o.status.code(), Some(0), "{}", describe(&o));
}

#[test]
fn acked_gate_test_changed_only_in_merge_commit_passes_hook_pipeline() {
    let d = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let base = fixture(d.path());
    change_f_in_merge_only(d.path());
    // Precondition: the range's tree diff does contain F (what gate-check sees).
    let diff = git(d.path(), &["diff", "--name-only", &base, "HEAD"]);
    assert!(
        diff.lines().any(|l| l == F),
        "fixture: F must be in the tree diff: {diff}"
    );
    let o = hook_pipeline(d.path(), home.path(), &base);
    assert_eq!(
        o.status.code(),
        Some(0),
        "a gate path changed only by a merge commit must be observed by `map build \
         --baseline <base>` (or its tracked ack honoured) exactly as the same change in an \
         ordinary commit is; got:\n{}",
        describe(&o)
    );
}
