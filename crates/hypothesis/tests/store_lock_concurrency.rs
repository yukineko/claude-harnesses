//! Two real `hypothesis` processes racing a load->mutate->save cycle against
//! DIFFERENT hypotheses in the SAME store must never lose either update. This
//! is the regression test for `crate::lock::StoreLock` as used by
//! `crate::store::Store` (backlog id ad8c09da): unlike `condukt::lock::RunLock`
//! (hard-skip semantics), `StoreLock` *waits* for a live holder to release, so
//! both concurrent CLI invocations must complete AND both mutations must land
//! — a lost update (last-writer-wins clobbering the whole file) is the bug
//! this lock exists to prevent.
//!
//! Repeated across several trials (fresh HOME each time) so the race window
//! is exercised more than once — a single trial could pass by luck even if
//! the lock were broken.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static TRIAL_COUNTER: AtomicU64 = AtomicU64::new(0);

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hypothesis"))
}

fn run(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin())
        .env("HOME", home)
        .args(args)
        .output()
        .expect("failed to run hypothesis")
}

fn spawn_confidence(home: &Path, id: &str, value: &str, delay_ms: &str) -> std::process::Child {
    Command::new(bin())
        .env("HOME", home)
        // Widens the load->mutate->save race window (held inside StoreLock)
        // past real process-spawn overhead so this test deterministically
        // forces the interleave instead of relying on two independently
        // scheduled OS processes to collide by chance. No-op in production.
        .env("HYPOTHESIS_TEST_LOAD_DELAY_MS", delay_ms)
        .args(["confidence", id, value])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn hypothesis confidence")
}

#[test]
fn concurrent_confidence_updates_to_different_hypotheses_do_not_lose_either_update() {
    const TRIALS: u64 = 8;

    for _ in 0..TRIALS {
        let n = TRIAL_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "hypothesis-store-lock-concurrency-{}-{n}",
            std::process::id()
        ));
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();

        // Seed two hypotheses (sequential; not part of the race).
        let out1 = run(&home, &["add", "h1"]);
        assert!(out1.status.success(), "trial {n}: add h1 failed: {out1:?}");
        let id1 = String::from_utf8_lossy(&out1.stdout).trim().to_string();

        let out2 = run(&home, &["add", "h2"]);
        assert!(out2.status.success(), "trial {n}: add h2 failed: {out2:?}");
        let id2 = String::from_utf8_lossy(&out2.stdout).trim().to_string();

        // Race two concurrent `confidence` updates to DIFFERENT hypotheses.
        // Each does load -> (artificial delay) -> mutate -> save; without
        // mutual exclusion around the whole cycle, the second save clobbers
        // the first's in-memory snapshot and one update is lost.
        let child_a = spawn_confidence(&home, &id1, "0.11", "150");
        let child_b = spawn_confidence(&home, &id2, "0.22", "150");

        let out_a = child_a.wait_with_output().expect("wait on confidence A");
        let out_b = child_b.wait_with_output().expect("wait on confidence B");

        assert!(
            out_a.status.success(),
            "trial {n}: confidence update A failed: stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&out_a.stdout),
            String::from_utf8_lossy(&out_a.stderr)
        );
        assert!(
            out_b.status.success(),
            "trial {n}: confidence update B failed: stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&out_b.stdout),
            String::from_utf8_lossy(&out_b.stderr)
        );

        let list_out = run(&home, &["list", "--json"]);
        assert!(
            list_out.status.success(),
            "trial {n}: list --json failed: {list_out:?}"
        );
        let items: serde_json::Value = serde_json::from_slice(&list_out.stdout)
            .unwrap_or_else(|e| panic!("trial {n}: list --json was not valid JSON ({e})"));
        let items = items.as_array().expect("list --json should be an array");

        let conf = |id: &str| -> f64 {
            items
                .iter()
                .find(|h| h["id"] == serde_json::json!(id))
                .unwrap_or_else(|| {
                    panic!("trial {n}: hypothesis {id} missing from list after race: {items:?}")
                })["confidence"]
                .as_f64()
                .unwrap_or_else(|| panic!("trial {n}: hypothesis {id} missing a confidence field"))
        };

        assert_eq!(
            conf(&id1),
            0.11,
            "trial {n}: {id1}'s confidence update was lost to a concurrent save"
        );
        assert_eq!(
            conf(&id2),
            0.22,
            "trial {n}: {id2}'s confidence update was lost to a concurrent save"
        );

        std::fs::remove_dir_all(&tmp).ok();
    }
}
