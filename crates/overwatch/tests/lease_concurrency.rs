//! Two real processes racing `overwatch begin` for the SAME lease key must
//! never both succeed: exactly one must win (exit 0) and the other must see
//! itself skipped (exit 1). This is the regression test for the TOCTOU
//! double-claim fixed by `crate::lock::LeaseLock` (hypothesis 9c733d74):
//! before that fix, two sessions racing `begin()`'s load->check->save cycle
//! could both pass the `is_held_by_other` check before either saved.
//!
//! Repeated across several trials (fresh HOME/cwd/key each time) so the race
//! window is exercised more than once — a single trial could pass by luck
//! even with the bug present.
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static TRIAL_COUNTER: AtomicU64 = AtomicU64::new(0);

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_overwatch"))
}

fn spawn_begin(
    cwd: &std::path::Path,
    home: &std::path::Path,
    key: &str,
    session: &str,
) -> std::process::Child {
    Command::new(bin())
        .current_dir(cwd)
        .env("HOME", home)
        // Widens the load->check->save race window past real process-spawn
        // overhead (see `lease::artificial_race_delay`'s doc comment) so this
        // test deterministically forces the interleave instead of relying on
        // two independently-scheduled OS processes to collide by chance.
        .env("OVERWATCH_TEST_BEGIN_DELAY_MS", "150")
        .args([
            "begin",
            "--key",
            key,
            "--title",
            "concurrent lease claim",
            "--session",
            session,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn overwatch begin")
}

/// Spawn `overwatch end --key <key>` with the mutator race-window widener set,
/// so its load->remove->save cycle straddles a racing `begin` for a DIFFERENT
/// key. Before the fix, `end` did its RMW without holding `LeaseLock`, so it
/// could load a pre-begin snapshot, sleep past the racing begin's save, then
/// save the stale snapshot back — silently freeing the just-claimed key.
fn spawn_end(cwd: &std::path::Path, home: &std::path::Path, key: &str) -> std::process::Child {
    Command::new(bin())
        .current_dir(cwd)
        .env("HOME", home)
        // Hold end()'s stale in-memory snapshot well past begin()'s save so the
        // (pre-fix) unlocked end() deterministically clobbers begin()'s claim.
        .env("OVERWATCH_TEST_MUTATOR_DELAY_MS", "500")
        .args(["end", "--key", key, "--status", "done"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn overwatch end")
}

/// Spawn `overwatch reassign --key <key> --to <to>` with the reassign
/// race-window widener set, so its load->remove->save cycle straddles a racing
/// `begin` for a DIFFERENT key. Before the fix, `reassign` (control.rs) did its
/// RMW without holding `LeaseLock` — the 6th lease mutator and the only one left
/// unlocked — so it could load a pre-begin snapshot, sleep past the racing
/// begin's save, then save the stale snapshot back, silently freeing the
/// just-claimed key.
fn spawn_reassign(
    cwd: &std::path::Path,
    home: &std::path::Path,
    key: &str,
    to: &str,
) -> std::process::Child {
    Command::new(bin())
        .current_dir(cwd)
        .env("HOME", home)
        // Hold reassign()'s stale in-memory snapshot well past begin()'s save so
        // the (pre-fix) unlocked reassign() deterministically clobbers begin()'s
        // claim.
        .env("OVERWATCH_TEST_REASSIGN_DELAY_MS", "500")
        .args(["reassign", "--key", key, "--to", to])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn overwatch reassign")
}

/// A concurrent, unlocked `reassign` must not clobber a just-committed begin.
///
/// Regression for backlog 0ee61602: `control::reassign` performed its
/// load->remove->save on `leases.json` WITHOUT holding `LeaseLock` (it was the
/// 6th lease mutator, the unlocked twin of the five locked in 7916a97d). A
/// racing `reassign key1` could load a pre-begin snapshot (no key2), let a
/// concurrent `begin key2` commit, then save its stale snapshot back — silently
/// freeing key2 so another begin could re-grab it (double-grab). Here we race
/// `reassign key1` against `begin key2` and assert key2 SURVIVES every trial.
/// Fails against the pre-fix (unlocked reassign) code; passes once reassign
/// holds `LeaseLock`.
#[test]
fn concurrent_reassign_does_not_clobber_racing_begin() {
    const TRIALS: u64 = 8;

    for _ in 0..TRIALS {
        let n = TRIAL_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "overwatch-lease-reassign-clobber-{}-{n}",
            std::process::id()
        ));
        let home = tmp.join("home");
        let cwd = tmp.join("cwd");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();

        let key1 = "victim-key-1";
        let key2 = "survivor-key-2";

        // Establish key1 so there is a live lease for `reassign` to release.
        let mut seed = spawn_begin(&cwd, &home, key1, "sess-a");
        assert!(
            seed.wait().expect("wait on seed begin").success(),
            "trial {n}: seeding begin(key1) should succeed"
        );

        // Race: reassign(key1) loads a snapshot WITHOUT key2 and sleeps 500ms;
        // begin(key2) loads, sleeps 150ms, then commits {key1,key2}. Without a
        // shared lock, reassign() wakes last and saves its stale snapshot back,
        // dropping key2.
        let mut child_reassign = spawn_reassign(&cwd, &home, key1, "sess-c");
        let mut child_begin = spawn_begin(&cwd, &home, key2, "sess-b");

        child_reassign.wait().expect("wait on reassign");
        assert!(
            child_begin.wait().expect("wait on begin").success(),
            "trial {n}: begin(key2) should win its own claim"
        );

        // Oracle: key2's holder (sess-b) must still have a live anchor. The
        // `lease` subcommand exits 0 with the lease when present, 1 when the
        // session holds nothing — so a non-zero exit means key2 was clobbered.
        let status = Command::new(bin())
            .current_dir(&cwd)
            .env("HOME", &home)
            .args(["lease", "--session", "sess-b", "--json"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("failed to query lease for sess-b");
        assert!(
            status.success(),
            "trial {n}: key2 ({key2}) was clobbered by the racing unlocked reassign() \
             (lease --session sess-b exited {:?})",
            status.code()
        );

        std::fs::remove_dir_all(&tmp).ok();
    }
}

/// A concurrent, unlocked lease mutator must not clobber a just-committed begin.
///
/// Regression for backlog 7916a97d: `run`/`end`/`heartbeat`/`reap` performed
/// their load->mutate->save on `leases.json` WITHOUT holding `LeaseLock` (only
/// `begin` did). A racing `end key1` could load a pre-begin snapshot (no key2),
/// let a concurrent `begin key2` commit, then save its stale snapshot back —
/// silently freeing key2 so another begin could re-grab it (the double-grab the
/// TOCTOU hardening was meant to close). Here we race `end key1` against
/// `begin key2` and assert key2 SURVIVES every trial. Fails against the pre-fix
/// (unlocked-mutator) code; passes once every mutator holds `LeaseLock`.
#[test]
fn concurrent_end_does_not_clobber_racing_begin() {
    const TRIALS: u64 = 8;

    for _ in 0..TRIALS {
        let n = TRIAL_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "overwatch-lease-mutator-clobber-{}-{n}",
            std::process::id()
        ));
        let home = tmp.join("home");
        let cwd = tmp.join("cwd");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();

        let key1 = "victim-key-1";
        let key2 = "survivor-key-2";

        // Establish key1 so there is a live lease for `end` to remove.
        let mut seed = spawn_begin(&cwd, &home, key1, "sess-a");
        assert!(
            seed.wait().expect("wait on seed begin").success(),
            "trial {n}: seeding begin(key1) should succeed"
        );

        // Race: end(key1) loads a snapshot WITHOUT key2 and sleeps 500ms;
        // begin(key2) loads, sleeps 150ms, then commits {key1,key2}. Without a
        // shared lock, end() wakes last and saves its stale snapshot back,
        // dropping key2.
        let mut child_end = spawn_end(&cwd, &home, key1);
        let mut child_begin = spawn_begin(&cwd, &home, key2, "sess-b");

        child_end.wait().expect("wait on end");
        assert!(
            child_begin.wait().expect("wait on begin").success(),
            "trial {n}: begin(key2) should win its own claim"
        );

        // Oracle: key2's holder (sess-b) must still have a live anchor. The
        // `lease` subcommand exits 0 with the lease when present, 1 when the
        // session holds nothing — so a non-zero exit means key2 was clobbered.
        let status = Command::new(bin())
            .current_dir(&cwd)
            .env("HOME", &home)
            .args(["lease", "--session", "sess-b", "--json"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("failed to query lease for sess-b");
        assert!(
            status.success(),
            "trial {n}: key2 ({key2}) was clobbered by the racing unlocked end() \
             (lease --session sess-b exited {:?})",
            status.code()
        );

        std::fs::remove_dir_all(&tmp).ok();
    }
}

#[test]
fn concurrent_begin_same_key_exactly_one_wins() {
    const TRIALS: u64 = 8;

    for _ in 0..TRIALS {
        let n = TRIAL_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "overwatch-lease-concurrency-{}-{n}",
            std::process::id()
        ));
        let home = tmp.join("home");
        let cwd = tmp.join("cwd");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();

        let key = "concurrency-key";

        // Back-to-back spawn (not wait-then-spawn) so both processes are
        // actually alive and racing the lease store at the same time.
        let mut child_a = spawn_begin(&cwd, &home, key, "sess-a");
        let mut child_b = spawn_begin(&cwd, &home, key, "sess-b");

        let status_a = child_a.wait().expect("wait on session-a");
        let status_b = child_b.wait().expect("wait on session-b");

        let codes = [status_a.code(), status_b.code()];
        let successes = codes.iter().filter(|c| **c == Some(0)).count();
        let skips = codes.iter().filter(|c| **c == Some(1)).count();

        assert_eq!(
            successes, 1,
            "trial {n}: expected exactly one winning begin() (exit 0), got codes {codes:?}"
        );
        assert_eq!(
            skips, 1,
            "trial {n}: expected exactly one skipped begin() (exit 1), got codes {codes:?}"
        );

        std::fs::remove_dir_all(&tmp).ok();
    }
}
