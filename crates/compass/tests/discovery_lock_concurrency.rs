//! Two real `compass` processes racing the shared discovery store's
//! load->mutate->save cycle must never lose either mutation. This is the
//! regression test for the advisory lock added to
//! `harness_core::discovery::{append_at, mark_selected_at}` (backlog id
//! 472a1483): `append_at`'s single `O_APPEND` write is atomic on its own, but
//! `mark_selected_at` reads a snapshot of the *whole* file and later rewrites
//! it wholesale (temp file + rename) — without a lock shared with `append_at`,
//! a rewrite that started before a concurrent append lands would silently
//! clobber it on rename (lost update), and two concurrent `select` calls could
//! likewise clobber each other's status flip.
//!
//! Repeated across several trials (fresh HOME/proj each time) so the race
//! window is exercised more than once — a single trial could pass by luck
//! even if the lock were broken.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

static TRIAL_COUNTER: AtomicU64 = AtomicU64::new(0);

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_compass"))
}

fn run(home: &Path, proj: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin())
        .current_dir(proj)
        .env("HOME", home)
        .args(args)
        .output()
        .expect("failed to run compass")
}

fn spawn_record(home: &Path, proj: &Path, title: &str) -> std::process::Child {
    Command::new(bin())
        .current_dir(proj)
        .env("HOME", home)
        .args([
            "discovery",
            "record",
            "--session-id",
            "racerA",
            "--title",
            title,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn compass discovery record")
}

fn spawn_select(home: &Path, proj: &Path, title: &str, delay_ms: &str) -> std::process::Child {
    Command::new(bin())
        .current_dir(proj)
        .env("HOME", home)
        // Widens the load->mutate->rewrite race window (held inside the
        // discovery store's advisory lock) past real process-spawn overhead
        // so this test deterministically forces the interleave instead of
        // relying on two independently scheduled OS processes to collide by
        // chance. No-op in production.
        .env("DISCOVERY_TEST_LOAD_DELAY_MS", delay_ms)
        .args(["discovery", "select", "--title", title])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn compass discovery select")
}

#[test]
fn concurrent_record_and_select_do_not_lose_either_mutation() {
    const TRIALS: u64 = 8;

    for _ in 0..TRIALS {
        let n = TRIAL_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "compass-discovery-lock-concurrency-{}-{n}",
            std::process::id()
        ));
        let home = tmp.join("home");
        let proj = tmp.join("proj");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&proj).unwrap();

        // Seed a pre-existing row that `select` will flip (sequential; not
        // part of the race).
        let seed_title = "pre-existing task";
        let seed = run(
            &home,
            &proj,
            &[
                "discovery",
                "record",
                "--session-id",
                "seed",
                "--title",
                seed_title,
            ],
        );
        assert!(
            seed.status.success(),
            "trial {n}: seeding pre-existing task failed: {seed:?}"
        );

        // Race a concurrent append (a NEW row) against a concurrent select
        // (rewriting the whole store to flip the seeded row).
        let race_title = format!("race task {n}");
        let child_record = spawn_record(&home, &proj, &race_title);
        let child_select = spawn_select(&home, &proj, seed_title, "150");

        let out_record = child_record
            .wait_with_output()
            .expect("wait on discovery record");
        let out_select = child_select
            .wait_with_output()
            .expect("wait on discovery select");

        assert!(
            out_record.status.success(),
            "trial {n}: discovery record failed: stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&out_record.stdout),
            String::from_utf8_lossy(&out_record.stderr)
        );
        assert!(
            out_select.status.success(),
            "trial {n}: discovery select failed: stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&out_select.stdout),
            String::from_utf8_lossy(&out_select.stderr)
        );

        let list_out = run(&home, &proj, &["discovery", "list", "--json"]);
        assert!(
            list_out.status.success(),
            "trial {n}: discovery list --json failed: {list_out:?}"
        );
        let rows: Value = serde_json::from_slice(&list_out.stdout)
            .unwrap_or_else(|e| panic!("trial {n}: list --json was not valid JSON ({e})"));
        let rows = rows.as_array().expect("list --json should be an array");

        let status_for = |title: &str| -> Option<String> {
            rows.iter()
                .find(|r| r["title"] == title)
                .and_then(|r| r["status"].as_str())
                .map(str::to_string)
        };

        assert_eq!(
            status_for(&race_title),
            Some("discovered".to_string()),
            "trial {n}: the concurrent append's row was lost to the concurrent select's \
             rewrite (rows after race: {rows:?})"
        );
        assert_eq!(
            status_for(seed_title),
            Some("selected".to_string()),
            "trial {n}: the concurrent select's status flip was lost (rows after race: {rows:?})"
        );

        std::fs::remove_dir_all(&tmp).ok();
    }
}
