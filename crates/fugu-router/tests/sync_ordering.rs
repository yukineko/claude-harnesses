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
//!
//! A second defect, measured 2026-10-02 at rev b478fdcf (fugu-router 0.1.31):
//! every message `sync` emits — the pure success/progress ones included — went
//! to **stderr**, and nothing at all went to stdout:
//!
//!     $ fugu-router sync 2>/dev/null       # stdout: EMPTY
//!     $ fugu-router sync 2>&1 >/dev/null   # stderr:
//!     pulling from remote…
//!     pull done.
//!     nothing to push (already up to date with the remote).
//!     # exit 0
//!
//! The only caller is a `SessionEnd` hook (`hooks/hooks.json`), whose stderr IS
//! surfaced to the user while its stdout is discarded. So a completely
//! successful sync looked like an error at the end of every single session, and
//! the user reported it as one. The fix is a split, not a silencing:
//! success/progress goes to stdout (silent at SessionEnd, still informative when
//! a human runs the command), stderr stays reserved for failures. Silencing
//! both halves would convert a noisy success into an invisible failure — the
//! fail-open this repository forbids (CLAUDE.md §1/§3) — so the tests below pin
//! BOTH directions.

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
/// A fake home whose `sync_repo` points at a path that does not exist, so the
/// clone phase — the first thing `sync` does — fails. Shared by every test that
/// needs a genuinely failing sync, so they all exercise the same failure mode.
fn unclonable_remote(name: &str) -> PathBuf {
    let root = scratch(name);
    let home = root.join("home");
    let cfg_dir = home.join(".fugu-router");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join("config.toml"),
        format!(
            "sync_repo = \"{}\"\n",
            root.join("does-not-exist.git").to_str().unwrap()
        ),
    )
    .unwrap();
    home
}

#[test]
fn a_failed_sync_is_surfaced_through_the_prompt_hook() {
    let home = unclonable_remote("failure");

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

// ---------------------------------------------------------------------------
// Which stream the messages go to. A SessionEnd hook's stderr reaches the user
// and its stdout is discarded, so the stream *is* the user-visible behaviour.
// ---------------------------------------------------------------------------

/// A sync that succeeded must put NOTHING on stderr — and must still say what
/// it did, on stdout. Both halves are asserted on purpose: a test that only
/// demanded an empty stderr would be satisfied by a command that went mute,
/// which trades a visible non-problem for an invisible one.
#[test]
fn a_successful_sync_is_silent_on_stderr_and_reports_progress_on_stdout() {
    let (home, _remote, _record) = steady_state("stream-success");

    let out = run(&home, &["sync"], None);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    // Precondition: this run really did succeed, so everything it printed is
    // success/progress text and none of it is a failure report.
    assert!(
        out.status.success(),
        "precondition failed: sync exited {:?} from the steady state, so this \
         test cannot say anything about a *successful* sync's streams.\n\
         stdout: {stdout:?}\nstderr: {stderr:?}",
        out.status.code()
    );

    assert_eq!(
        stderr.trim(),
        "",
        "a SUCCESSFUL `fugu-router sync` wrote to stderr. Its only caller is a \
         SessionEnd hook whose stderr is surfaced to the user, so this exact \
         text is what makes a clean sync look like an error at the end of every \
         session (the reported symptom). stderr must be reserved for failures.\n\
         observed stderr: {stderr:?}\nobserved stdout: {stdout:?}"
    );

    for expected in [
        "committed: fugu-router sync",
        "pulling from remote",
        "pull done.",
        "pushed local records.",
    ] {
        assert!(
            stdout.contains(expected),
            "sync's progress/success message {expected:?} is missing from \
             stdout. Moving stderr's noise nowhere instead of to stdout would \
             make a human running the command by hand see nothing at all — the \
             fix is a stream split, not a silencing.\n\
             observed stdout: {stdout:?}\nobserved stderr: {stderr:?}"
        );
    }
}

/// The verbatim case from the bug report: the second, no-op run. The hook fires
/// at every session end, so this is the output the user actually saw most of the
/// time — three progress lines on stderr with exit 0.
#[test]
fn a_noop_sync_is_silent_on_stderr_and_reports_on_stdout() {
    let (home, _remote, _record) = steady_state("stream-noop");

    let first = run(&home, &["sync"], None);
    assert!(
        first.status.success(),
        "precondition failed: the first sync exited {:?}\nstdout: {}\nstderr: {}",
        first.status.code(),
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );

    let out = run(&home, &["sync"], None);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "precondition failed: the repeat sync exited {:?}\nstdout: {stdout:?}\nstderr: {stderr:?}",
        out.status.code()
    );

    assert_eq!(
        stderr.trim(),
        "",
        "the no-op repeat sync — the common case at SessionEnd — still wrote to \
         stderr. Measured 2026-10-02 the user saw exactly this: \"pulling from \
         remote… / pull done. / nothing to push (already up to date with the \
         remote).\" with exit 0, and read it as an error.\n\
         observed stderr: {stderr:?}\nobserved stdout: {stdout:?}"
    );

    for expected in [
        "pulling from remote",
        "pull done.",
        "nothing to push (already up to date with the remote)",
    ] {
        assert!(
            stdout.contains(expected),
            "the no-op sync's message {expected:?} is on neither stream; it \
             must move to stdout, not disappear.\n\
             observed stdout: {stdout:?}\nobserved stderr: {stderr:?}"
        );
    }
}

/// The other direction, and it matters just as much: a sync that FAILED must
/// still be loud on stderr. Reuses `unclonable_remote`, the same failure mode
/// `a_failed_sync_is_surfaced_through_the_prompt_hook` already pins, so the two
/// halves of the property are asserted against one measured failure — not an
/// invented one.
#[test]
fn a_failed_sync_still_writes_to_stderr() {
    let home = unclonable_remote("stream-failure");

    let out = run(&home, &["sync"], None);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    assert!(
        !out.status.success(),
        "precondition failed: sync against an unclonable remote exited 0, so \
         this test is not observing a failure at all.\n\
         stdout: {stdout:?}\nstderr: {stderr:?}"
    );

    assert!(
        !stderr.trim().is_empty(),
        "a FAILED `fugu-router sync` wrote nothing to stderr. SessionEnd \
         discards a hook's stdout and surfaces its stderr, so moving the \
         failure report to stdout along with the progress text would turn the \
         one thing the user must see into silence — a visible non-problem \
         traded for an invisible real one (CLAUDE.md §1/§3).\n\
         observed stdout: {stdout:?}\nobserved stderr: {stderr:?}"
    );
    assert!(
        stderr.contains("clone"),
        "sync failed in the clone phase, but stderr does not mention the clone \
         failure — whatever it did print is not the failure report, so the \
         actual cause is still dark.\n\
         observed stderr: {stderr:?}\nobserved stdout: {stdout:?}"
    );
}
