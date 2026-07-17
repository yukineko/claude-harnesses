//! Capstone end-to-end test for `condukt::schedule::schedule()` (the
//! deterministic scheduler in `crates/condukt/src/schedule.rs`), driven
//! entirely through the REAL `condukt` binary (`env!("CARGO_BIN_EXE_condukt")`)
//! via its `schedule --file <decomp.json>` subcommand — the same
//! real-binary-driving style `foundation_capstone_e2e.rs` uses.
//!
//! Proves the file-conflict scheduling contract end-to-end with a single
//! synthetic decomposition mixing three kinds of task pairs:
//!
//! 1. an exact-duplicate-path conflict pair (`touched_files` identical, byte
//!    for byte) — must be forced serial (never share a parallel batch);
//! 2. a normalize_entry-only conflict pair (different raw spellings —
//!    `./`-prefix and doubled `/` — that only compare equal AFTER
//!    `normalize_entry`, added in commit e238391) — must ALSO be forced
//!    serial, proving the normalization is load-bearing and not just a unit
//!    detail;
//! 3. a genuinely independent pair (disjoint files) — must be allowed to
//!    share a parallel batch (no over-serialization regression).
//!
//! Step 3 of the task also simulates the actual worker/merge pipeline
//! consequence of that schedule: each task is materialized as a write to a
//! path under a shared tempdir, sequenced according to the computed
//! schedule (serial tasks run one at a time in order; parallel-batch tasks
//! run "concurrently" via OS threads writing simultaneously), and the final
//! on-disk content is asserted to be exactly what each task intended to
//! write — no data loss/clobbering for either conflict pair, and the
//! independent pair still lands both files correctly when run in parallel.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::{Arc, Barrier};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

/// A throwaway workspace: a decomposition JSON on disk plus a scratch dir the
/// simulated worker writes land in.
struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let pid = std::process::id();
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-schedule-capstone-e2e-{pid}-{tag}"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        Self { dir: base }
    }

    fn write_decomp(&self, json: &serde_json::Value) -> PathBuf {
        let p = self.dir.join("decomp.json");
        std::fs::write(&p, serde_json::to_string(json).unwrap()).unwrap();
        p
    }

    /// The scratch "repo" tasks simulate writing into.
    fn workdir(&self) -> PathBuf {
        let p = self.dir.join("work");
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn schedule(&self, decomp_path: &PathBuf) -> Output {
        Command::new(bin())
            .arg("schedule")
            .arg("--file")
            .arg(decomp_path)
            .output()
            .expect("spawn condukt schedule")
    }
}

/// Build the synthetic decomposition described in done_criteria:
///   - dup-a / dup-b: identical touched_files path -> exact conflict.
///   - norm-a / norm-b: different raw spellings that normalize_entry collapses
///     to the same path ("./shared/norm.rs" vs "shared//norm.rs").
///   - indep-a / indep-b: disjoint files -> genuinely independent.
fn synthetic_decomp() -> serde_json::Value {
    serde_json::json!({
        "goal": "schedule conflict capstone",
        "tasks": [
            {
                "id": "dup-a",
                "title": "dup-a",
                "touched_files": ["shared/dup.rs"],
                "deps": [],
                "class": "parallel"
            },
            {
                "id": "dup-b",
                "title": "dup-b",
                "touched_files": ["shared/dup.rs"],
                "deps": [],
                "class": "parallel"
            },
            {
                "id": "norm-a",
                "title": "norm-a",
                "touched_files": ["./shared/norm.rs"],
                "deps": [],
                "class": "parallel"
            },
            {
                "id": "norm-b",
                "title": "norm-b",
                "touched_files": ["shared//norm.rs"],
                "deps": [],
                "class": "parallel"
            },
            {
                "id": "indep-a",
                "title": "indep-a",
                "touched_files": ["indep/a.rs"],
                "deps": [],
                "class": "parallel"
            },
            {
                "id": "indep-b",
                "title": "indep-b",
                "touched_files": ["indep/b.rs"],
                "deps": [],
                "class": "parallel"
            }
        ]
    })
}

/// Parsed shape of the `Schedule` the binary prints (mirrors
/// `crates/condukt/src/model.rs::Schedule`; kept minimal/local to this test so
/// it doesn't need a lib target import).
#[derive(Debug, serde::Deserialize)]
struct ScheduleOut {
    batches: Vec<BatchOut>,
    serial: Vec<String>,
    #[allow(dead_code)]
    gated: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
struct BatchOut {
    parallel: Vec<String>,
}

fn batch_containing<'a>(sched: &'a ScheduleOut, id: &str) -> Option<&'a BatchOut> {
    sched
        .batches
        .iter()
        .find(|b| b.parallel.iter().any(|p| p == id))
}

#[test]
fn conflicting_pairs_are_forced_serial_and_independent_pair_stays_parallel() {
    let fx = Fixture::new("assertions");
    let decomp_path = fx.write_decomp(&synthetic_decomp());

    let out = fx.schedule(&decomp_path);
    assert!(
        out.status.success(),
        "condukt schedule must exit 0: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let sched: ScheduleOut = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "schedule output must parse as JSON: {e}; stdout={}",
            String::from_utf8_lossy(&out.stdout)
        )
    });

    // ── 1. Exact-duplicate-path conflict pair: never share a batch ────────
    assert!(
        sched.serial.contains(&"dup-a".to_string()) && sched.serial.contains(&"dup-b".to_string()),
        "exact-duplicate-path pair must both be forced serial; serial={:?}, batches={:?}",
        sched.serial,
        sched.batches
    );
    assert!(
        batch_containing(&sched, "dup-a").is_none() && batch_containing(&sched, "dup-b").is_none(),
        "exact-duplicate-path pair must not appear in any parallel batch; batches={:?}",
        sched.batches
    );

    // ── 2. normalize_entry-only conflict pair: also never share a batch ───
    // ("./shared/norm.rs" vs "shared//norm.rs" only compare equal AFTER
    // normalize_entry strips the "./" prefix and collapses "//" -> "/".)
    assert!(
        sched.serial.contains(&"norm-a".to_string())
            && sched.serial.contains(&"norm-b".to_string()),
        "normalize_entry-equivalent pair must both be forced serial; serial={:?}, batches={:?}",
        sched.serial,
        sched.batches
    );
    assert!(
        batch_containing(&sched, "norm-a").is_none()
            && batch_containing(&sched, "norm-b").is_none(),
        "normalize_entry-equivalent pair must not appear in any parallel batch; batches={:?}",
        sched.batches
    );

    // ── 3. Independent pair: allowed to share a parallel batch ─────────────
    assert!(
        !sched.serial.contains(&"indep-a".to_string())
            && !sched.serial.contains(&"indep-b".to_string()),
        "independent pair must not be forced serial; serial={:?}",
        sched.serial
    );
    let ba = batch_containing(&sched, "indep-a");
    let bb = batch_containing(&sched, "indep-b");
    assert!(
        ba.is_some() && bb.is_some(),
        "independent pair must both appear in some parallel batch; batches={:?}",
        sched.batches
    );
    assert!(
        ba.unwrap().parallel.contains(&"indep-b".to_string()),
        "independent pair must share the SAME parallel batch (parallel allowed); batches={:?}",
        sched.batches
    );
}

/// Step 3 of done_criteria: simulate the actual worker/merge pipeline
/// consequence of the computed schedule against a shared tempdir, and prove
/// no data loss happens for either conflict pair while the independent pair
/// genuinely benefits from concurrent writes.
#[test]
fn schedule_driven_write_simulation_has_no_data_loss_for_conflict_pairs() {
    let fx = Fixture::new("writes");
    let decomp_path = fx.write_decomp(&synthetic_decomp());
    let work = fx.workdir();

    let out = fx.schedule(&decomp_path);
    assert!(
        out.status.success(),
        "condukt schedule must exit 0: {out:?}"
    );
    let sched: ScheduleOut = serde_json::from_slice(&out.stdout).expect("schedule JSON parses");

    // Each task's simulated "worker write": file path (relative to `work`,
    // matching touched_files) and the content it intends to write. dup-a/
    // dup-b and norm-a/norm-b share a real path, so if they were EVER allowed
    // to run concurrently the later write could clobber/interleave with the
    // earlier one and we could observe a torn/partial file. Because the
    // scheduler forces both conflicting pairs onto `serial` (proven above),
    // this simulation runs them one-at-a-time in serial order, and the file
    // must end up holding exactly the LAST serial writer's content deterministically.
    let mut file_for: HashMap<&str, &str> = HashMap::new();
    file_for.insert("dup-a", "shared/dup.rs");
    file_for.insert("dup-b", "shared/dup.rs");
    file_for.insert("norm-a", "shared/norm.rs"); // normalized landing spot
    file_for.insert("norm-b", "shared/norm.rs");
    file_for.insert("indep-a", "indep/a.rs");
    file_for.insert("indep-b", "indep/b.rs");

    let content_for = |id: &str| -> String { format!("content-from-{id}\n") };

    // Run serial tasks first, strictly in the schedule's serial order (this
    // is what the real merge pipeline does: serial tasks land on the main
    // line one at a time before/between parallel batches).
    for id in &sched.serial {
        let rel = file_for[id.as_str()];
        let path = work.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content_for(id)).unwrap();
    }

    // dup-a/dup-b and norm-a/norm-b are both entirely serial-scheduled (per
    // the scheduling assertions above), so after the serial loop each shared
    // path holds exactly its LAST writer's content, untorn.
    let dup_final = std::fs::read_to_string(work.join("shared/dup.rs")).unwrap();
    assert_eq!(
        dup_final,
        content_for("dup-b"),
        "serial-ordered writes to the duplicate path must not be lost/clobbered; got {dup_final:?}"
    );
    let norm_final = std::fs::read_to_string(work.join("shared/norm.rs")).unwrap();
    assert_eq!(
        norm_final,
        content_for("norm-b"),
        "serial-ordered writes to the normalize_entry-equivalent path must not be lost/clobbered; got {norm_final:?}"
    );

    // Now run every parallel batch: each batch's tasks fire their writes
    // truly concurrently (a `Barrier` forces every thread in the batch to
    // start writing at the same instant), simulating real parallel workers
    // landing their changes at once. Because indep-a/indep-b write to
    // disjoint files, concurrent execution must not lose or corrupt either.
    for batch in &sched.batches {
        let barrier = Arc::new(Barrier::new(batch.parallel.len().max(1)));
        let mut handles = Vec::new();
        for id in &batch.parallel {
            let rel = file_for[id.as_str()].to_string();
            let content = content_for(id);
            let work = work.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                let path = work.join(&rel);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).unwrap();
                }
                barrier.wait();
                std::fs::write(&path, content).unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
    }

    // Both independent files must have landed exactly their own task's
    // content — concurrent writes to disjoint files never interfere.
    let a_final = std::fs::read_to_string(work.join("indep/a.rs")).unwrap();
    assert_eq!(
        a_final,
        content_for("indep-a"),
        "indep-a's parallel write must land intact"
    );
    let b_final = std::fs::read_to_string(work.join("indep/b.rs")).unwrap();
    assert_eq!(
        b_final,
        content_for("indep-b"),
        "indep-b's parallel write must land intact"
    );

    // The conflict-pair files must be untouched by the parallel phase (they
    // were fully consumed by the serial phase above) and still hold exactly
    // their last serial writer's content -- no data loss end to end.
    let dup_after_all = std::fs::read_to_string(work.join("shared/dup.rs")).unwrap();
    assert_eq!(dup_after_all, content_for("dup-b"));
    let norm_after_all = std::fs::read_to_string(work.join("shared/norm.rs")).unwrap();
    assert_eq!(norm_after_all, content_for("norm-b"));
}
