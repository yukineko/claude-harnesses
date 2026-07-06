//! Cross-crate E2E integration tests pinning the flow-pipeline "integration
//! contract". Every test spawns the REAL built workspace binaries via
//! `std::process::Command` — no in-process linking, no mocks — so a regression
//! in how `flow` shells out to `backlog`, or how `fugu-router route` /
//! `condukt schedule` / `condukt state` shape their I/O, breaks a test here.
//!
//! Fail-soft binary discovery: if a required sibling binary has not been built,
//! the test prints a skip note and returns green (see [`bin`]). Build the bins
//! first (`cargo build -p flow -p backlog -p fugu-router -p condukt`) for the
//! tests to actually exercise the contract instead of skipping.
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
/// joined with `../../target`. Returns `None` if the binary is not built — every
/// test treats `None` as "skip", never a failure, so the suite stays green on a
/// machine where the bins aren't compiled.
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

/// The directory that holds the built binaries (parent of a resolved binary).
/// Used to prepend to a child's `PATH` so `flow` can find `backlog` by name.
fn bin_dir() -> Option<PathBuf> {
    bin("flow").and_then(|p| p.parent().map(PathBuf::from))
}

/// Build a child `PATH` value with `dir` prepended to the inherited `PATH`.
fn path_with(dir: &std::path::Path) -> std::ffi::OsString {
    let mut prefix = dir.as_os_str().to_os_string();
    if let Some(existing) = std::env::var_os("PATH") {
        prefix.push(":");
        prefix.push(existing);
    }
    prefix
}

// ---------------------------------------------------------------------------
// Contract A — directive injection (`flow propose`)
//
// `flow propose` (fn propose in crates/flow/src/main.rs) shells out to the
// `backlog` binary on PATH:
//     backlog list --project <cwd> --status pending --json
// and injects conditionally:
//   * 0 pending items         → prints NOTHING (stay silent).
//   * N >= 1 pending items    → prints "[flow] バックログに {N} 件 ... '{title}' ..."
//   * backlog absent/errored  → prints the static English DIRECTIVE const.
// ---------------------------------------------------------------------------

/// Empty project (no backlog items) → `flow propose` prints nothing.
///
/// Runs `flow propose` with CWD = a fresh tempdir and a fresh `$HOME` (so the
/// spawned `backlog` sees an empty store), with the bin dir prepended to PATH so
/// `flow` finds the real `backlog`. Asserts stdout (trimmed) is empty.
#[test]
fn contract_a_empty_project_is_silent() {
    let (Some(flow), Some(dir)) = (bin("flow"), bin_dir()) else {
        eprintln!("SKIP contract_a_empty_project_is_silent: flow binary not built");
        return;
    };
    let proj = TempDir::new().expect("tempdir proj");
    let home = TempDir::new().expect("tempdir home");

    let out = Command::new(&flow)
        .arg("propose")
        .current_dir(proj.path())
        .env("HOME", home.path())
        .env("PATH", path_with(&dir))
        .output()
        .expect("spawn flow propose");

    assert!(
        out.status.success(),
        "flow propose must exit 0 (never break a turn); status={:?} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.trim().is_empty(),
        "empty project must produce silent stdout, got: {stdout:?}",
    );
}

/// Pending item present → `flow propose` prints the count and the task title.
///
/// Seeds the backlog with a real `backlog add --title ZZTASK --project <proj>
/// --priority p1` (verified flags), sharing an isolated `$HOME` between the seed
/// and the `flow propose` run (both read `$HOME/.backlog`). Asserts the injected
/// summary contains the count `1 件` and the substring `ZZTASK`.
#[test]
fn contract_a_pending_item_is_announced() {
    let (Some(flow), Some(backlog), Some(dir)) = (bin("flow"), bin("backlog"), bin_dir()) else {
        eprintln!("SKIP contract_a_pending_item_is_announced: flow/backlog binary not built");
        return;
    };
    let proj = TempDir::new().expect("tempdir proj");
    let home = TempDir::new().expect("tempdir home");
    // Canonicalize: on macOS `$TMPDIR` lives under `/var/...` which is a symlink
    // to `/private/var/...`. `flow` scopes its backlog query by
    // `std::env::current_dir()`, which returns the physical (canonical) path, so
    // `backlog add --project` must use the same canonical form or the item won't
    // be visible to `flow propose`.
    let proj_canon = proj.path().canonicalize().expect("canonicalize proj");
    let proj_str = proj_canon.to_string_lossy().to_string();

    let add = Command::new(&backlog)
        .args([
            "add",
            "--title",
            "ZZTASK",
            "--project",
            &proj_str,
            "--priority",
            "p1",
        ])
        .env("HOME", home.path())
        .output()
        .expect("spawn backlog add");
    assert!(
        add.status.success(),
        "backlog add must succeed; stderr={}",
        String::from_utf8_lossy(&add.stderr),
    );

    let out = Command::new(&flow)
        .arg("propose")
        .current_dir(&proj_canon)
        .env("HOME", home.path())
        .env("PATH", path_with(&dir))
        .output()
        .expect("spawn flow propose");
    assert!(out.status.success(), "flow propose must exit 0");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("1 件"),
        "propose must report the pending count '1 件', got: {stdout:?}",
    );
    assert!(
        stdout.contains("ZZTASK"),
        "propose must name the top task 'ZZTASK', got: {stdout:?}",
    );
}

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
        eprintln!("SKIP contract_b_route_preserves_ids_and_models: fugu-router not built");
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
        eprintln!("SKIP contract_b_schedule_routes_classes: fugu-router/condukt not built");
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
        eprintln!("SKIP contract_b_state_roundtrip: condukt not built");
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
        eprintln!("SKIP contract_c_connected_chain: fugu-router/condukt not built");
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
