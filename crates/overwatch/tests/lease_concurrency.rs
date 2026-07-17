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
