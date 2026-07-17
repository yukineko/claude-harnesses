//! Two real processes racing `condukt state claim-task` for the SAME hashkey
//! from DIFFERENT runs must never both succeed: exactly one must claim it
//! (exit 0) and the other must be hard-skipped (exit 1). This is the
//! regression test for `crate::lock::RunLock` as used by
//! `crate::claim::claim_tasks` — the load->check->save cycle it serializes
//! (backlog id 8a71b681): `RunLock` was ported from `backlog::lock` /
//! `overwatch::lock` but, unlike those two, had no dedicated multi-process
//! concurrency test proving the port itself is correct end-to-end (only
//! single-process unit tests in `claim.rs` exercised it).
//!
//! Repeated across several trials (fresh HOME/cwd each time) so the race
//! window is exercised more than once — a single trial could pass by luck
//! even if the lock were broken.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static TRIAL_COUNTER: AtomicU64 = AtomicU64::new(0);

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_condukt"))
}

fn write_decomp(repo: &Path, name: &str, hashkey: &str) -> PathBuf {
    let p = repo.join(name);
    let json = format!(
        r#"{{"goal":"race {hashkey}","tasks":[{{"id":"t1","title":"race {hashkey}","touched_files":[],"deps":[],"class":"parallel","done_criteria":"d"}}]}}"#
    );
    std::fs::write(&p, json).unwrap();
    p
}

fn spawn_claim(
    cwd: &Path,
    home: &Path,
    run: &str,
    hashkey: &str,
    session: &str,
) -> std::process::Child {
    Command::new(bin())
        .current_dir(cwd)
        .env("HOME", home)
        // Widens the load->check->save race window (held inside RunLock) past
        // real process-spawn overhead so this test deterministically forces
        // the interleave instead of relying on two independently-scheduled OS
        // processes to collide by chance. No-op in production.
        .env("CONDUKT_TEST_CLAIM_DELAY_MS", "150")
        .args([
            "state",
            "claim-task",
            "--run",
            run,
            "--session",
            session,
            "--title",
            "concurrent claim",
            "--hashkey",
            hashkey,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn condukt state claim-task")
}

#[test]
fn concurrent_claim_task_same_hashkey_different_runs_exactly_one_wins() {
    const TRIALS: u64 = 8;

    for _ in 0..TRIALS {
        let n = TRIAL_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "condukt-run-lock-concurrency-{}-{n}",
            std::process::id()
        ));
        let home = tmp.join("home");
        let cwd = tmp.join("cwd");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();

        let hashkey = "concurrency-hashkey";
        write_decomp(&cwd, "dec.json", hashkey);

        // Back-to-back spawn (not wait-then-spawn) so both processes are
        // actually alive and racing the claims registry at the same time.
        let child_a = spawn_claim(&cwd, &home, "runA", hashkey, "sess-a");
        let child_b = spawn_claim(&cwd, &home, "runB", hashkey, "sess-b");

        let out_a = child_a.wait_with_output().expect("wait on runA");
        let out_b = child_b.wait_with_output().expect("wait on runB");

        let codes = [out_a.status.code(), out_b.status.code()];
        let successes = codes.iter().filter(|c| **c == Some(0)).count();
        let skips = codes.iter().filter(|c| **c == Some(1)).count();

        assert_eq!(
            successes, 1,
            "trial {n}: expected exactly one winning claim-task (exit 0), got codes {codes:?}"
        );
        assert_eq!(
            skips, 1,
            "trial {n}: expected exactly one hard-skipped claim-task (exit 1), got codes {codes:?}"
        );

        // Exit codes alone are not proof of correct hard-skip semantics: a
        // shared-tmp-file lost-update bug (a *different* regression from the
        // TOCTOU this test targets) can ALSO make the loser exit 1 — but via a
        // crash (empty stdout, rename ENOENT on stderr), not a real skip JSON.
        // Assert on stdout content so a crash can never masquerade as a valid
        // hard skip.
        let (winner, loser) = if out_a.status.code() == Some(0) {
            (&out_a, &out_b)
        } else {
            (&out_b, &out_a)
        };
        let winner_json: serde_json::Value =
            serde_json::from_slice(&winner.stdout).unwrap_or_else(|e| {
                panic!(
                    "trial {n}: winner stdout was not valid JSON ({e}): {:?}",
                    String::from_utf8_lossy(&winner.stdout)
                )
            });
        assert_eq!(
            winner_json["claimed"],
            serde_json::json!([hashkey]),
            "trial {n}: winner should have claimed '{hashkey}', got: {winner_json}"
        );
        assert_eq!(
            winner_json["skipped"],
            serde_json::json!([]),
            "trial {n}: winner should have no skips, got: {winner_json}"
        );

        let loser_json: serde_json::Value =
            serde_json::from_slice(&loser.stdout).unwrap_or_else(|e| {
                panic!(
                    "trial {n}: loser stdout was not valid skip JSON ({e}) — likely crashed \
                     instead of being cleanly hard-skipped: stdout={:?} stderr={:?}",
                    String::from_utf8_lossy(&loser.stdout),
                    String::from_utf8_lossy(&loser.stderr)
                )
            });
        assert_eq!(
            loser_json["claimed"],
            serde_json::json!([]),
            "trial {n}: loser should have claimed nothing, got: {loser_json}"
        );
        let skipped = loser_json["skipped"].as_array().unwrap_or_else(|| {
            panic!("trial {n}: loser JSON missing 'skipped' array: {loser_json}")
        });
        assert_eq!(
            skipped.len(),
            1,
            "trial {n}: loser should have exactly one skip entry, got: {loser_json}"
        );
        assert_eq!(
            skipped[0]["file"], hashkey,
            "trial {n}: loser's skip entry should name the raced hashkey, got: {loser_json}"
        );

        std::fs::remove_dir_all(&tmp).ok();
    }
}
