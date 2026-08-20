//! Cross-crate E2E integration tests pinning the flow-pipeline "integration
//! contract". Every test spawns the REAL built workspace binaries via
//! `std::process::Command` — no in-process linking, no mocks — so a regression
//! in how `fugu-router route` / `condukt schedule` / `condukt state` shape their
//! I/O breaks a test here. (Until 2026-08-20 it also covered how `flow propose`
//! shelled out to `backlog`; that binary and its nudge are retired — see
//! Contract A below.)
//!
//! Binary discovery ([`bin`]): if a required sibling binary has not been
//! built, local (non-CI) runs print a skip note and return green — a
//! convenience so `cargo test -p integration-tests` doesn't force a full
//! workspace build during iteration. In CI (`CI` env var set, as GitHub
//! Actions and most CI providers do), a missing binary is instead a hard
//! `panic!` failure — see [`skip_or_panic`] — so a broken build step can never
//! silently downgrade this suite to a no-op in the gate. Build
//! the bins first (`cargo build -p backlog -p fugu-router -p condukt`)
//! for the tests to actually exercise the contract instead of skipping.
//!
//! Isolation: every test uses `tempfile::TempDir` for both the project dir and
//! (where a binary persists state under `$HOME`) a fresh `$HOME`, so nothing
//! touches the developer's real `~/.backlog` or `~/.condukt` state. No network.

use std::path::PathBuf;
use std::process::Command;

use tempfile::TempDir;

/// Resolve a sibling workspace binary by name, honouring `CARGO_TARGET_DIR`.
///
/// Looks under `<target>/release/<name>` first, then `<target>/debug/<name>`,
/// returning the first that exists. When `CARGO_TARGET_DIR` is unset the target
/// dir is derived from this crate's manifest dir (`.../crates/integration-tests`)
/// joined with `../../target`. Returns `None` if the binary is not built.
///
/// `None` is only ever treated as "skip" OUTSIDE CI (see [`skip_or_panic`]) —
/// in CI a missing binary is a hard failure, never a silent skip.
fn bin(name: &str) -> Option<PathBuf> {
    let target = match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"),
    };
    for profile in ["release", "debug"] {
        let candidate = target.join(profile).join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// True when running under a CI provider.
///
/// Checks the `CI` environment variable, the de-facto convention GitHub
/// Actions (and most other CI providers) set to `"true"` for every job. Any
/// non-empty value counts as "in CI" — we only care that *some* CI marker is
/// present, not its exact spelling.
fn is_ci() -> bool {
    std::env::var_os("CI").is_some_and(|v| !v.is_empty())
}

/// Handle a missing-binary condition for a test: fail hard in CI, skip locally.
///
/// In CI (`is_ci()`), a missing binary means the build step that is supposed to
/// produce it is broken or missing — that must fail the gate, never silently
/// downgrade to a skip. Locally, missing binaries are a normal part of
/// iterating on a single crate, so we print a skip note and let the caller
/// `return` early (green).
///
/// `test_name` and `msg` are only used for the message; call this then `return`
/// from the test when it does not panic.
fn skip_or_panic(test_name: &str, msg: &str) {
    if is_ci() {
        panic!("{test_name}: {msg} (CI must build all required binaries before running e2e tests)");
    }
    eprintln!("SKIP {test_name}: {msg}");
}

// ---------------------------------------------------------------------------
// Contract A — RETIRED 2026-08-20 (`flow propose`)
//
// Two tests here pinned the SessionStart directive injection: 0 pending items →
// silent, N ≥ 1 → "[flow] バックログに {N} 件 … '{title}' …", backlog absent → a
// static fallback directive. The nudge was retired on the user's instruction and
// `flow` is a skills-only plugin now with no binary at all, so there is no
// `flow propose` to spawn. Deleted because the subject is gone, NOT because it
// went red.
//
// What still covers the neighbourhood: `flow_skill_queue_contract.rs` (moved out
// of `crates/flow` in the same change) pins the /flow SKILL's queue-driving text,
// and Contract B below still spawns the real backlog/fugu-router/condukt
// binaries, so the inter-binary contracts this file exists for are unaffected.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Contract B — routing + schedule + state
// ---------------------------------------------------------------------------

/// A 3-task decomposition with one task of each schedulable class.
fn decomposition_json() -> serde_json::Value {
    serde_json::json!({
        "goal": "integration contract",
        "tasks": [
            {
                "id": "tpar",
                "title": "parallel task",
                "class": "parallel",
                "touched_files": ["a.rs"],
                "done_criteria": "builds",
                "suggested_model": "sonnet"
            },
            {
                "id": "tser",
                "title": "serial task",
                "class": "serial",
                "touched_files": ["b.rs"],
                "done_criteria": "builds",
                "suggested_model": "sonnet"
            },
            {
                "id": "tgate",
                "title": "gated task",
                "class": "gated",
                "touched_files": ["c.rs"],
                "done_criteria": "builds",
                "suggested_model": "sonnet"
            }
        ]
    })
}

/// `fugu-router route` preserves every task id, assigns each a valid model, and
/// writes a valid-JSON `--report`.
///
/// Feeds the decomposition via `--file`, captures routed JSON on stdout, and
/// asserts: all three ids survive; every `suggested_model` is one of
/// haiku/sonnet/opus; the `--report` file exists and parses as JSON. Runs with
/// CWD = a fresh tempdir so routing memory is defaulted, not machine-specific.
#[test]
fn contract_b_route_preserves_ids_and_models() {
    let Some(router) = bin("fugu-router") else {
        skip_or_panic(
            "contract_b_route_preserves_ids_and_models",
            "fugu-router not built",
        );
        return;
    };
    let tmp = TempDir::new().expect("tempdir");
    let dpath = tmp.path().join("d.json");
    let rpath = tmp.path().join("r.json");
    std::fs::write(&dpath, decomposition_json().to_string()).expect("write d.json");

    let out = Command::new(&router)
        .args(["route", "--file"])
        .arg(&dpath)
        .arg("--report")
        .arg(&rpath)
        .current_dir(tmp.path())
        .output()
        .expect("spawn fugu-router route");
    assert!(
        out.status.success(),
        "route must exit 0; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );

    let routed: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("route stdout is valid JSON");
    let tasks = routed["tasks"]
        .as_array()
        .expect("routed .tasks is an array");
    assert_eq!(tasks.len(), 3, "all 3 tasks must survive routing");

    let mut ids: Vec<&str> = tasks.iter().map(|t| t["id"].as_str().unwrap()).collect();
    ids.sort_unstable();
    assert_eq!(ids, ["tgate", "tpar", "tser"], "every task id must survive");

    for t in tasks {
        let model = t["suggested_model"]
            .as_str()
            .expect("each task has a suggested_model string");
        assert!(
            ["haiku", "sonnet", "opus"].contains(&model),
            "suggested_model must be haiku/sonnet/opus, got {model:?} for {}",
            t["id"],
        );
    }

    assert!(rpath.is_file(), "route must write the --report file");
    let report_bytes = std::fs::read(&rpath).expect("read report");
    let _report: serde_json::Value =
        serde_json::from_slice(&report_bytes).expect("--report file is valid JSON");
}

/// `condukt schedule` places each class where the contract requires.
///
/// Routes the decomposition first (so the input mirrors the real pipeline), then
/// pipes the routed JSON into `condukt schedule --file`. Asserts the `gated` id
/// is under `gated`; the `serial` id is under `serial` OR named in `warnings`
/// (a serial demotion is an accepted equivalent — the contract is "never in a
/// parallel batch"); the `parallel` id appears somewhere under `batches`.
#[test]
fn contract_b_schedule_routes_classes() {
    let (Some(router), Some(condukt)) = (bin("fugu-router"), bin("condukt")) else {
        skip_or_panic(
            "contract_b_schedule_routes_classes",
            "fugu-router/condukt not built",
        );
        return;
    };
    let tmp = TempDir::new().expect("tempdir");
    let dpath = tmp.path().join("d.json");
    let routed_path = tmp.path().join("routed.json");
    std::fs::write(&dpath, decomposition_json().to_string()).expect("write d.json");

    let route = Command::new(&router)
        .args(["route", "--file"])
        .arg(&dpath)
        .current_dir(tmp.path())
        .output()
        .expect("spawn route");
    assert!(route.status.success(), "route must exit 0");
    std::fs::write(&routed_path, &route.stdout).expect("write routed.json");

    let sched_out = Command::new(&condukt)
        .args(["schedule", "--file"])
        .arg(&routed_path)
        .output()
        .expect("spawn condukt schedule");
    assert!(
        sched_out.status.success(),
        "schedule must exit 0; stderr={}",
        String::from_utf8_lossy(&sched_out.stderr),
    );

    let sched: serde_json::Value =
        serde_json::from_slice(&sched_out.stdout).expect("schedule stdout is valid JSON");

    // Collect ids from each schedule bucket.
    let collect = |key: &str| -> Vec<String> {
        sched[key]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let gated = collect("gated");
    let serial = collect("serial");
    let warnings = collect("warnings");
    let batched: Vec<String> = sched["batches"]
        .as_array()
        .map(|batches| {
            batches
                .iter()
                .flat_map(|b| b["parallel"].as_array().cloned().unwrap_or_default())
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // A gated task is quarantined under `gated`.
    assert!(
        gated.iter().any(|id| id == "tgate"),
        "gated task 'tgate' must appear under `gated`, got gated={gated:?}",
    );

    // A serial task must never land in a parallel batch: it is either listed
    // under `serial` or demoted with a note in `warnings` (either is accepted).
    let serial_ok =
        serial.iter().any(|id| id == "tser") || warnings.iter().any(|w| w.contains("tser"));
    assert!(
        serial_ok,
        "serial task 'tser' must be under `serial` or noted in `warnings`; \
         serial={serial:?} warnings={warnings:?}",
    );
    assert!(
        !batched.iter().any(|id| id == "tser"),
        "serial task 'tser' must NOT be scheduled into a parallel batch; batched={batched:?}",
    );

    // A parallel task is scheduled into a batch.
    assert!(
        batched.iter().any(|id| id == "tpar"),
        "parallel task 'tpar' must appear under `batches`, got batched={batched:?}",
    );
}

/// Full state roundtrip: `state init` → `state set ... verified` (x3) →
/// `state gate` reflects all-verified.
///
/// Runs `condukt` with `$HOME` set to a fresh tempdir so all run-state is written
/// under the temp HOME (never the developer's `~/.condukt`). Parses the run id
/// from the LAST `run-...` line of `state init` stdout, marks every task
/// `verified`, then asserts `state gate --run <RID>` exits 0 (the run is
/// complete). A negative control confirms the gate FAILS before all tasks are
/// verified, so the pass is meaningful. (Note: `state gate` emits its human
/// "gate PASS/FAIL" line on stderr; the machine contract is the exit code.)
#[test]
fn contract_b_state_roundtrip_gate_passes_when_all_verified() {
    let Some(condukt) = bin("condukt") else {
        skip_or_panic("contract_b_state_roundtrip", "condukt not built");
        return;
    };
    let tmp = TempDir::new().expect("tempdir");
    let home = TempDir::new().expect("tempdir home");
    let dpath = tmp.path().join("d.json");
    std::fs::write(&dpath, decomposition_json().to_string()).expect("write d.json");

    let run_condukt = |args: &[&str]| -> std::process::Output {
        Command::new(&condukt)
            .args(args)
            .env("HOME", home.path())
            .output()
            .expect("spawn condukt")
    };

    // init: prints a human line then the bare run id on its own last line.
    let init = Command::new(&condukt)
        .args(["state", "init", "--file"])
        .arg(&dpath)
        .env("HOME", home.path())
        .output()
        .expect("spawn condukt state init");
    assert!(
        init.status.success(),
        "state init must exit 0; stderr={}",
        String::from_utf8_lossy(&init.stderr),
    );
    let init_stdout = String::from_utf8_lossy(&init.stdout);
    let rid = init_stdout
        .lines()
        .map(str::trim)
        .rev()
        .find(|l| l.starts_with("run-"))
        .expect("state init must print a run-... id on its own line")
        .to_string();

    // Negative control: gate must FAIL while tasks are still pending.
    let gate_before = run_condukt(&["state", "gate", "--run", &rid]);
    assert!(
        !gate_before.status.success(),
        "gate must FAIL before any task is verified (negative control)",
    );

    // Mark every task verified.
    for task in ["tpar", "tser", "tgate"] {
        let set = run_condukt(&[
            "state", "set", "--run", &rid, "--task", task, "--status", "verified",
        ]);
        assert!(
            set.status.success(),
            "state set {task} verified must exit 0; stderr={}",
            String::from_utf8_lossy(&set.stderr),
        );
    }

    // gate: now the run is complete → exit 0, and stdout announces the pass.
    let gate = run_condukt(&["state", "gate", "--run", &rid]);
    assert!(
        gate.status.success(),
        "gate must PASS once all tasks are verified; stdout={} stderr={}",
        String::from_utf8_lossy(&gate.stdout),
        String::from_utf8_lossy(&gate.stderr),
    );
    // The gate's human verdict is on stderr ("gate PASS: run '<rid>' complete").
    let gate_msg = String::from_utf8_lossy(&gate.stderr);
    assert!(
        gate_msg.contains("PASS") || gate_msg.contains("complete"),
        "gate should announce completion on stderr, got: {gate_msg:?}",
    );
}

// ---------------------------------------------------------------------------
// Contract C — connected 3-binary chain (route → schedule → state → gate)
//
// Contracts A and B each pin a SINGLE hop in isolation. This test threads the
// WHOLE pipeline: the output of `fugu-router route` is fed verbatim into BOTH
// `condukt schedule` AND `condukt state init` (one routed artifact, reused), and
// the run is driven to a passing gate using ONLY the task ids the SCHEDULER
// emits. It pins the integration contract the isolated hops miss — that every
// stage agrees on the same task set: routed JSON is a valid schedule *and* state
// input, and the ids the scheduler batches/quarantines are exactly the ids state
// tracks and the gate requires. A drift in any hop's task-set handling (a
// dropped id, a renamed field, a bucket that silently loses a task) breaks this
// even when each hop still passes its own isolated contract.
// ---------------------------------------------------------------------------

/// Every routed task id lands in exactly one schedule bucket — no drops, no
/// extras. Union of `serial`, `gated`, and each batch's `parallel` list.
fn scheduled_ids(sched: &serde_json::Value) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for key in ["serial", "gated"] {
        if let Some(a) = sched[key].as_array() {
            ids.extend(a.iter().filter_map(|v| v.as_str().map(String::from)));
        }
    }
    if let Some(batches) = sched["batches"].as_array() {
        for b in batches {
            if let Some(par) = b["parallel"].as_array() {
                ids.extend(par.iter().filter_map(|v| v.as_str().map(String::from)));
            }
        }
    }
    ids.sort_unstable();
    ids
}

/// route → schedule → state init → gate, threaded through one routed artifact.
///
/// Stage 1 (`fugu-router route`) is the boundary where the LLM decomposition
/// hands off to the deterministic layer; its stdout is persisted once as
/// `routed.json` and every later stage consumes THAT file (not a fresh
/// decomposition), so the test exercises the real inter-binary data flow.
/// Asserts the routed task set survives the route→schedule hop exactly, that the
/// same routed artifact initialises run-state, and that marking every
/// SCHEDULER-emitted id verified drives the gate to pass. Fresh `$HOME` isolates
/// all run-state; fail-soft skip if the bins are not built.
#[test]
fn contract_c_connected_chain_route_schedule_state_gate() {
    let (Some(router), Some(condukt)) = (bin("fugu-router"), bin("condukt")) else {
        skip_or_panic(
            "contract_c_connected_chain",
            "fugu-router/condukt not built",
        );
        return;
    };
    let tmp = TempDir::new().expect("tempdir");
    let home = TempDir::new().expect("tempdir home");
    let dpath = tmp.path().join("d.json");
    let routed_path = tmp.path().join("routed.json");
    std::fs::write(&dpath, decomposition_json().to_string()).expect("write d.json");

    // Stage 1: route. Persist the routed artifact ONCE; it is the single source
    // threaded through every later stage.
    let route = Command::new(&router)
        .args(["route", "--file"])
        .arg(&dpath)
        .current_dir(tmp.path())
        .output()
        .expect("spawn route");
    assert!(
        route.status.success(),
        "route must exit 0; stderr={}",
        String::from_utf8_lossy(&route.stderr),
    );
    std::fs::write(&routed_path, &route.stdout).expect("write routed.json");
    let routed: serde_json::Value =
        serde_json::from_slice(&route.stdout).expect("routed stdout is valid JSON");
    let mut routed_ids: Vec<String> = routed["tasks"]
        .as_array()
        .expect("routed .tasks is an array")
        .iter()
        .map(|t| t["id"].as_str().expect("task id").to_string())
        .collect();
    routed_ids.sort_unstable();
    assert_eq!(
        routed_ids,
        ["tgate", "tpar", "tser"],
        "sanity: the three decomposition ids must survive routing",
    );

    // Stage 2: schedule consumes the SAME routed artifact. The full task set must
    // survive the route→schedule hop — every routed id in exactly one bucket.
    let sched_out = Command::new(&condukt)
        .args(["schedule", "--file"])
        .arg(&routed_path)
        .output()
        .expect("spawn condukt schedule");
    assert!(
        sched_out.status.success(),
        "schedule must exit 0; stderr={}",
        String::from_utf8_lossy(&sched_out.stderr),
    );
    let sched: serde_json::Value =
        serde_json::from_slice(&sched_out.stdout).expect("schedule stdout is valid JSON");
    let sched_ids = scheduled_ids(&sched);
    assert_eq!(
        sched_ids, routed_ids,
        "route→schedule must preserve the exact task set (no dropped/extra ids); \
         routed={routed_ids:?} scheduled={sched_ids:?}",
    );

    // Stage 3: state init consumes the SAME routed artifact (proving routed JSON
    // is a valid STATE input, not only a schedule input).
    let init = Command::new(&condukt)
        .args(["state", "init", "--file"])
        .arg(&routed_path)
        .env("HOME", home.path())
        .output()
        .expect("spawn condukt state init");
    assert!(
        init.status.success(),
        "state init must exit 0 on the routed artifact; stderr={}",
        String::from_utf8_lossy(&init.stderr),
    );
    let init_stdout = String::from_utf8_lossy(&init.stdout);
    let rid = init_stdout
        .lines()
        .map(str::trim)
        .rev()
        .find(|l| l.starts_with("run-"))
        .expect("state init must print a run-... id")
        .to_string();

    // Stage 4: drive the run to completion using ONLY the ids the SCHEDULER
    // emitted. If any hop disagreed on the id set, a `state set` (unknown task)
    // or the final gate (an untracked/unverified task) would fail here.
    for id in &sched_ids {
        let set = Command::new(&condukt)
            .args([
                "state", "set", "--run", &rid, "--task", id, "--status", "verified",
            ])
            .env("HOME", home.path())
            .output()
            .expect("spawn condukt state set");
        assert!(
            set.status.success(),
            "state set {id} verified must exit 0 (id came from the scheduler); stderr={}",
            String::from_utf8_lossy(&set.stderr),
        );
    }
    let gate = Command::new(&condukt)
        .args(["state", "gate", "--run", &rid])
        .env("HOME", home.path())
        .output()
        .expect("spawn condukt state gate");
    assert!(
        gate.status.success(),
        "gate must PASS once every scheduler-emitted task is verified; stdout={} stderr={}",
        String::from_utf8_lossy(&gate.stdout),
        String::from_utf8_lossy(&gate.stderr),
    );
}
