// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `cmd_sync` — the order of operations, and what happens when it cannot finish.
//!
//! `sync` used to pull BEFORE committing local appends, with `--ff-only`:
//!
//!     git pull --ff-only        # then: add -u, commit, push
//!
//! Both halves of that abort in the store's *normal steady state*. The store
//! files live inside the sync dir, so `record` leaves them as uncommitted
//! working-tree modifications; and any second machine pushing makes the
//! histories diverge. Measured 2026-08-26 against a copy of the real
//! `~/.fugu-router/record-repo` (local 62dcc8da, remote 9b8b6271):
//!
//!     $ git pull --ff-only
//!     error: Your local changes to the following files would be overwritten
//!            by merge: episodes.jsonl / playbooks.jsonl
//!     Aborting                                                     # exit 1
//!
//!     # and even after committing the local appends first:
//!     $ git pull --ff-only
//!     fatal: Not possible to fast-forward, aborting.               # exit 128
//!     $ git pull --no-rebase
//!     Auto-merging episodes.jsonl / playbooks.jsonl -> no conflict  # exit 0
//!
//! `anyhow::ensure!(out.status.success(), "git pull failed")` turned either
//! into an abort that never reached the push phase — and because the only
//! caller is a `SessionEnd` hook, whose exit code and stderr reach neither the
//! agent nor the user, the abort was invisible. 34 days of episodes sat
//! unpushed on one machine while nothing anywhere said so (CLAUDE.md §1/§3).
//!
//! Reordering alone was not enough, and these tests are what showed it: with
//! commit-then-merge in place, `sync_pushes_local_appends_when_the_remote_has_advanced`
//! still failed with `CONFLICT (content): Merge conflict in episodes.jsonl`.
//! Two machines appending to the same JSONL land their additions adjacent at
//! end-of-file, which the default text driver cannot resolve — the real store
//! auto-merged only because its two sides happened to be far apart. Hence
//! `merge=union` in the sync dir's `.gitattributes`: it keeps BOTH sides'
//! lines, and the store is already deduplicated by content hash, so a
//! duplicate line is recoverable where a dropped episode is not.
//!
//! These tests pin the three properties that were missing: sync completes from
//! the steady state, it stays a no-op on repeat (the hook fires every session),
//! and when it genuinely cannot complete it does not fail into silence.

use std::path::{Path, PathBuf};
use std::process::Command;

const EXE: &str = env!("CARGO_BIN_EXE_fugu-router");

/// A git invocation that must succeed, run with a deterministic identity so
/// the test does not depend on the developer's `~/.gitconfig` (the fake HOME
/// these tests install has none).
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} could not run: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} in {} failed ({:?}):\n{}\n{}",
        dir.display(),
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn append_line(path: &Path, line: &str) {
    let mut body = std::fs::read_to_string(path).unwrap_or_default();
    body.push_str(line);
    body.push('\n');
    std::fs::write(path, body).unwrap();
}

/// A unique scratch root. `std::env::temp_dir()` plus the test name keeps the
/// cases independent without pulling in a tempdir dependency.
fn scratch(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("fugu-sync-test-{name}-{}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root).unwrap();
    }
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// Run the built binary with `HOME` pointed at a fake home, so
/// `config::home_dir()` (via `dirs::home_dir()`, which honours `$HOME` on
/// Linux) resolves the config and the sync dir inside the scratch root.
fn run(home: &Path, args: &[&str], stdin: Option<&str>) -> std::process::Output {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = Command::new(EXE)
        .args(args)
        .env("HOME", home)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("could not spawn {EXE}: {e}"));
    if let Some(body) = stdin {
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(body.as_bytes())
            .unwrap();
    }
    drop(child.stdin.take());
    child.wait_with_output().unwrap()
}

/// Builds the steady state the old code could not survive: the record repo has
/// uncommitted local appends AND the remote has advanced from another machine.
/// Returns (fake home, bare remote path, record-repo path).
fn steady_state(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = scratch(name);
    let home = root.join("home");
    let cfg_dir = home.join(".fugu-router");
    std::fs::create_dir_all(&cfg_dir).unwrap();

    // A bare "GitHub".
    let remote = root.join("remote.git");
    std::fs::create_dir_all(&remote).unwrap();
    git(&remote, &["init", "--bare", "--initial-branch=main", "."]);

    // Seed it through a throwaway clone.
    let seed = root.join("seed");
    git(&root, &["clone", remote.to_str().unwrap(), "seed"]);
    git(&seed, &["config", "user.name", "t"]);
    git(&seed, &["config", "user.email", "t@example.com"]);
    append_line(&seed.join("episodes.jsonl"), r#"{"task":"seed"}"#);
    append_line(&seed.join("playbooks.jsonl"), r#"{"task":"seed"}"#);
    git(&seed, &["add", "-A"]);
    git(&seed, &["commit", "-m", "seed"]);
    git(&seed, &["push", "origin", "main"]);

    // The machine under test.
    let record = cfg_dir.join("record-repo");
    git(
        &cfg_dir,
        &["clone", remote.to_str().unwrap(), "record-repo"],
    );
    git(&record, &["config", "user.name", "t"]);
    git(&record, &["config", "user.email", "t@example.com"]);

    // Another machine pushes -> the histories will diverge.
    let other = root.join("other");
    git(&root, &["clone", remote.to_str().unwrap(), "other"]);
    git(&other, &["config", "user.name", "t"]);
    git(&other, &["config", "user.email", "t@example.com"]);
    append_line(
        &other.join("episodes.jsonl"),
        r#"{"task":"from-other-machine"}"#,
    );
    git(&other, &["add", "-u"]);
    git(&other, &["commit", "-m", "other machine"]);
    git(&other, &["push", "origin", "main"]);

    // This machine records an episode: an uncommitted working-tree append,
    // because store_path() points inside the sync dir when sync_repo is set.
    append_line(
        &record.join("episodes.jsonl"),
        r#"{"task":"from-this-machine"}"#,
    );

    std::fs::write(
        cfg_dir.join("config.toml"),
        format!("sync_repo = \"{}\"\n", remote.to_str().unwrap()),
    )
    .unwrap();

    (home, remote, record)
}

/// The whole point of `sync`: an episode recorded here reaches the remote,
/// even though the remote moved on in the meantime. This is the steady state,
/// not an edge case — it is what every second machine produces.
#[test]
fn sync_pushes_local_appends_when_the_remote_has_advanced() {
    let (home, remote, _record) = steady_state("push");

    let out = run(&home, &["sync"], None);
    assert!(
        out.status.success(),
        "`fugu-router sync` exited {:?} from the store's steady state \
         (uncommitted local appends + an advanced remote). Nothing downstream \
         of a SessionEnd hook can see this, so the store just stops syncing.\n\
         stdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // Read the pushed content straight out of the bare remote.
    let pushed = git(&remote, &["show", "main:episodes.jsonl"]);
    assert!(
        pushed.contains("from-this-machine"),
        "sync reported success but this machine's episode never reached the \
         remote. Pushed episodes.jsonl:\n{pushed}"
    );
    assert!(
        pushed.contains("from-other-machine"),
        "sync dropped the other machine's episode — it must merge, not \
         overwrite. Pushed episodes.jsonl:\n{pushed}"
    );
}

/// Running twice must be a no-op the second time, not a failure: the hook
/// fires on every session end.
#[test]
fn sync_is_idempotent() {
    let (home, _remote, _record) = steady_state("idempotent");

    let first = run(&home, &["sync"], None);
    assert!(first.status.success(), "first sync failed");

    let second = run(&home, &["sync"], None);
    assert!(
        second.status.success(),
        "second sync exited {:?}; a SessionEnd hook runs every session, so a \
         no-op run must succeed.\nstderr: {}",
        second.status.code(),
        String::from_utf8_lossy(&second.stderr)
    );
}

/// When sync genuinely cannot finish, it must not fail into silence. A
/// non-zero exit is not enough: the only caller is a `SessionEnd` hook, whose
/// exit code and stderr reach nobody. The failure has to survive into a
/// channel someone actually reads — here, the `UserPromptSubmit` hook that
/// this plugin already owns.
#[test]
fn a_failed_sync_is_surfaced_through_the_prompt_hook() {
    let root = scratch("failure");
    let home = root.join("home");
    let cfg_dir = home.join(".fugu-router");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    // A remote that cannot be cloned: the clone phase must fail.
    std::fs::write(
        cfg_dir.join("config.toml"),
        format!(
            "sync_repo = \"{}\"\n",
            root.join("does-not-exist.git").to_str().unwrap()
        ),
    )
    .unwrap();

    let sync = run(&home, &["sync"], None);
    assert!(
        !sync.status.success(),
        "sync against an unclonable remote exited 0 — an unreachable remote is \
         not a synced store (CLAUDE.md §3: cannot-determine resolves to the \
         restrictive side)"
    );

    let payload = r#"{"session_id":"s1","prompt":"refactor the parser in src/lib.rs"}"#;
    let prompt = run(&home, &["prompt"], Some(payload));
    let stdout = String::from_utf8_lossy(&prompt.stdout);
    assert!(
        stdout.contains("sync"),
        "the prompt hook said nothing about the failed sync, so the failure is \
         still dark: a SessionEnd stderr nobody reads is the only trace.\n\
         prompt stdout: {stdout:?}"
    );
}

/// ...and the notice must clear once sync succeeds, or it becomes noise that
/// gets ignored — which is the same as being invisible.
#[test]
fn the_failure_notice_clears_after_a_successful_sync() {
    let (home, remote, record) = steady_state("clears");

    // Force a failure in the pull phase by breaking the remote. Note this
    // edits the checkout's `origin`, NOT `sync_repo` in config.toml: once the
    // sync dir is a checkout, `sync_repo` is only consulted for the initial
    // clone and `origin` is what pull/push actually use.
    git(
        &record,
        &["remote", "set-url", "origin", "/nonexistent/not-here.git"],
    );
    let failed = run(&home, &["sync"], None);
    assert!(
        !failed.status.success(),
        "expected an unreachable origin to fail:\nstderr: {}",
        String::from_utf8_lossy(&failed.stderr)
    );

    let payload = r#"{"session_id":"s1","prompt":"refactor the parser in src/lib.rs"}"#;
    let noticed = run(&home, &["prompt"], Some(payload));
    assert!(
        String::from_utf8_lossy(&noticed.stdout).contains("sync"),
        "precondition failed: the failure was not surfaced at all"
    );

    // ...then restore the good remote and sync for real.
    git(
        &record,
        &["remote", "set-url", "origin", remote.to_str().unwrap()],
    );
    let ok = run(&home, &["sync"], None);
    assert!(
        ok.status.success(),
        "recovery sync failed:\n{}",
        String::from_utf8_lossy(&ok.stderr)
    );

    let after = run(&home, &["prompt"], Some(payload));
    let stdout = String::from_utf8_lossy(&after.stdout);
    assert!(
        !stdout.contains("could not sync"),
        "the stale failure notice survived a successful sync; a warning that \
         never clears is one nobody reads.\nprompt stdout: {stdout:?}"
    );
}
