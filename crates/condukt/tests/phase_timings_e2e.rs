//! End-to-end coverage for the per-phase timestamps on `TaskState` and the
//! `state timings` subcommand. Spawns the real `condukt` binary against an
//! isolated HOME so it exercises the ACTUAL orchestrator transitions
//! (`state set --status running|done|failed`, `worktree merge`) and the on-disk
//! run-state JSON, not a hand-built struct.
//!
//! Contract under test:
//!   - `running` stamps `worker_started_at` (set-once).
//!   - the first `done` stamps `worker_ended_at` AND `verifier_started_at`.
//!   - `verified`/`failed` stamps `verifier_ended_at`.
//!   - a successful `worktree merge --run --task` stamps `merge_completed_at`.
//!   - `state timings` renders an unmeasured phase as an explicit `unmeasured`
//!     marker (text) / `null` (JSON), NEVER as 0 — so never-measured stays
//!     distinct from completed-instantly.
//!   - run-state JSON written before these fields existed still loads.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

struct Fixture {
    repo: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let pid = std::process::id();
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-phase-e2e-{pid}-{tag}"));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        run_git(&repo, &["init", "-q", "-b", "main"]);
        run_git(&repo, &["config", "user.email", "t@t.t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-q", "-m", "init"]);
        Self { repo, home }
    }

    fn condukt(&self, args: &[&str]) -> Output {
        Command::new(bin())
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_SESSION_ID", "sess-test")
            // Pin the default branch to this repo's branch so an ambient
            // CONDUKT_DEFAULT_BRANCH inherited from the caller's shell cannot
            // steer merge/reconcile at a foreign ref.
            .env("CONDUKT_DEFAULT_BRANCH", "main")
            .output()
            .expect("spawn condukt")
    }

    fn write_decomp(&self, name: &str, task_id: &str, file: &str) -> PathBuf {
        let p = self.repo.join(name);
        let json = format!(
            r#"{{"goal":"g","tasks":[{{"id":"{task_id}","title":"edit {file}","touched_files":["{file}"],"deps":[],"class":"parallel","done_criteria":"d"}}]}}"#
        );
        std::fs::write(&p, json).unwrap();
        p
    }

    fn init_run(&self, run: &str, task_id: &str, file: &str) {
        let dec = self.write_decomp(&format!("{run}-dec.json"), task_id, file);
        let out = self.condukt(&[
            "state",
            "init",
            "--run",
            run,
            "--file",
            dec.to_str().unwrap(),
        ]);
        assert_eq!(
            out.status.code(),
            Some(0),
            "state init must succeed: {out:?}"
        );
    }

    fn set_status(&self, run: &str, task: &str, status: &str) {
        let out = self.condukt(&[
            "state", "set", "--run", run, "--task", task, "--status", status,
        ]);
        assert_eq!(
            out.status.code(),
            Some(0),
            "state set --status {status} must succeed: {out:?}"
        );
    }

    fn run_state_path(&self, run: &str) -> PathBuf {
        let target = format!("{run}.json");
        let state_root = self.home.join(".condukt").join("state");
        find_file(&state_root, &target)
            .unwrap_or_else(|| panic!("run-state file {target} not found under {state_root:?}"))
    }

    fn read_state(&self, run: &str) -> serde_json::Value {
        let txt = std::fs::read_to_string(self.run_state_path(run)).unwrap();
        serde_json::from_str(&txt).unwrap()
    }

    fn write_state(&self, run: &str, val: &serde_json::Value) {
        let path = self.run_state_path(run);
        std::fs::write(path, serde_json::to_string_pretty(val).unwrap()).unwrap();
    }
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let rd = std::fs::read_dir(dir).ok()?;
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let Some(hit) = find_file(&p, name) {
                return Some(hit);
            }
        } else if p.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(p);
        }
    }
    None
}

fn run_git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

/// The `running` transition (the real worker-dispatch edge) stamps
/// `worker_started_at`, set-once.
#[test]
fn phase_running_transition_stamps_worker_start() {
    let fx = Fixture::new("wstart");
    fx.init_run("runW", "t1", "src/a.rs");

    let before = fx.read_state("runW");
    assert!(
        before["tasks"][0]["worker_started_at"].is_null(),
        "worker_started_at must be absent before any running transition"
    );

    fx.set_status("runW", "t1", "running");
    let after = fx.read_state("runW");
    let ws = after["tasks"][0]["worker_started_at"].as_i64();
    assert!(
        ws.is_some(),
        "running must stamp worker_started_at (got {:?})",
        after["tasks"][0]["worker_started_at"]
    );

    // Set-once: a re-dispatch (…→running) must not move it.
    fx.set_status("runW", "t1", "done");
    fx.set_status("runW", "t1", "running");
    let again = fx.read_state("runW");
    assert_eq!(
        again["tasks"][0]["worker_started_at"].as_i64(),
        ws,
        "a later running transition must not overwrite worker_started_at"
    );
}

/// The first `done` transition stamps BOTH `worker_ended_at` and
/// `verifier_started_at` on the same edge.
#[test]
fn phase_done_transition_stamps_worker_end_and_verifier_start() {
    let fx = Fixture::new("wend");
    fx.init_run("runD", "t1", "src/b.rs");
    fx.set_status("runD", "t1", "running");

    let before = fx.read_state("runD");
    assert!(
        before["tasks"][0]["worker_ended_at"].is_null()
            && before["tasks"][0]["verifier_started_at"].is_null(),
        "worker_ended_at/verifier_started_at must be absent before done"
    );

    fx.set_status("runD", "t1", "done");
    let after = fx.read_state("runD");
    let we = after["tasks"][0]["worker_ended_at"].as_i64();
    let vs = after["tasks"][0]["verifier_started_at"].as_i64();
    assert!(
        we.is_some() && vs.is_some(),
        "done must stamp worker_ended_at and verifier_started_at (got we={:?} vs={:?})",
        after["tasks"][0]["worker_ended_at"],
        after["tasks"][0]["verifier_started_at"]
    );
    assert_eq!(
        we, vs,
        "worker_ended_at and verifier_started_at are stamped on the same edge"
    );

    // Set-once: a re-run of `set --status done` must not move worker_ended_at.
    fx.set_status("runD", "t1", "done");
    let again = fx.read_state("runD");
    assert_eq!(
        again["tasks"][0]["worker_ended_at"].as_i64(),
        we,
        "a repeated done must not move worker_ended_at"
    );
}

/// A verdict transition (`failed` here; `verified` takes the same code path)
/// stamps `verifier_ended_at`.
#[test]
fn phase_verdict_transition_stamps_verifier_end() {
    let fx = Fixture::new("vend");
    fx.init_run("runV", "t1", "src/c.rs");
    fx.set_status("runV", "t1", "running");
    fx.set_status("runV", "t1", "done");

    let before = fx.read_state("runV");
    assert!(
        before["tasks"][0]["verifier_ended_at"].is_null(),
        "verifier_ended_at must be absent before a verdict"
    );

    fx.set_status("runV", "t1", "failed");
    let after = fx.read_state("runV");
    assert!(
        after["tasks"][0]["verifier_ended_at"].as_i64().is_some(),
        "a verdict (failed) must stamp verifier_ended_at (got {:?})",
        after["tasks"][0]["verifier_ended_at"]
    );
}

/// A successful `worktree merge --run --task` stamps `merge_completed_at`.
#[test]
fn phase_successful_merge_stamps_merge_completed() {
    let fx = Fixture::new("merge");
    fx.init_run("runM", "t1", "src/d.rs");

    // Build a mergeable branch off main.
    run_git(&fx.repo, &["checkout", "-q", "-b", "condukt/runM-t1"]);
    std::fs::write(fx.repo.join("d.txt"), "work\n").unwrap();
    run_git(&fx.repo, &["add", "."]);
    run_git(&fx.repo, &["commit", "-q", "-m", "task work"]);
    run_git(&fx.repo, &["checkout", "-q", "main"]);

    let before = fx.read_state("runM");
    assert!(
        before["tasks"][0]["merge_completed_at"].is_null(),
        "merge_completed_at must be absent before merge"
    );

    let out = fx.condukt(&[
        "worktree",
        "merge",
        "--branch",
        "condukt/runM-t1",
        "--run",
        "runM",
        "--task",
        "t1",
    ]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "worktree merge must exit 0: {out:?}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("merged"),
        "merge must report success (got stdout: {stdout:?})"
    );

    let after = fx.read_state("runM");
    let mc = after["tasks"][0]["merge_completed_at"].as_i64();
    assert!(
        mc.is_some(),
        "a successful merge must stamp merge_completed_at (got {:?})",
        after["tasks"][0]["merge_completed_at"]
    );
}

/// `state timings` renders an unmeasured phase as an explicit marker, NEVER as
/// 0 seconds — the whole point of keeping the phase timestamps tri-state.
#[test]
fn phase_timings_renders_unmeasured_not_zero() {
    let fx = Fixture::new("unmeasured");
    fx.init_run("runT", "t1", "src/e.rs");
    // Only the worker START is measured; every other phase is unmeasured.
    fx.set_status("runT", "t1", "running");

    // JSON: worker span is null (worker_ended_at unmeasured), NOT 0.
    let out = fx.condukt(&["state", "timings", "--run", "runT", "--json"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "timings --json must exit 0: {out:?}"
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("timings JSON parses");
    let t0 = &v["tasks"][0];
    assert!(
        t0["worker_started_at"].as_i64().is_some(),
        "worker_started_at is measured"
    );
    assert!(
        t0["worker_secs"].is_null(),
        "an unmeasured worker span must be null, never 0 (got {:?})",
        t0["worker_secs"]
    );
    assert!(
        t0["verifier_secs"].is_null() && t0["verify_to_merge_secs"].is_null(),
        "unmeasured verifier/merge spans must be null, never 0"
    );

    // Text: shows the explicit `unmeasured` marker, and does NOT print `0s`.
    let out = fx.condukt(&["state", "timings", "--run", "runT"]);
    assert_eq!(out.status.code(), Some(0), "timings must exit 0: {out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("unmeasured"),
        "text output must render the unmeasured marker (got: {text:?})"
    );
    assert!(
        !text.contains("0s"),
        "an unmeasured phase must NOT be printed as 0s (got: {text:?})"
    );

    // Contrast: a fully-measured phase yields a real number (not the marker).
    // Drive worker start→end far enough apart is not needed for the type-level
    // check; a done edge makes worker_secs a concrete measured value.
    fx.set_status("runT", "t1", "done");
    let out = fx.condukt(&["state", "timings", "--run", "runT", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        v["tasks"][0]["worker_secs"].as_i64().is_some(),
        "once both worker endpoints are stamped, worker_secs is a measured number"
    );
}

/// BACK-COMPAT: run-state JSON written before any of the phase fields existed
/// (all absent) still deserializes, operates, and reports every phase as
/// unmeasured.
#[test]
fn phase_legacy_run_state_without_new_fields_still_loads() {
    let fx = Fixture::new("legacy");
    fx.init_run("runL", "t1", "src/f.rs");

    // Emulate a pre-field on-disk record: strip all phase fields (and started_at)
    // so none of the new keys are present.
    let mut r = fx.read_state("runL");
    if let Some(obj) = r["tasks"][0].as_object_mut() {
        for k in [
            "worker_started_at",
            "worker_ended_at",
            "verifier_started_at",
            "verifier_ended_at",
            "merge_completed_at",
            "started_at",
        ] {
            obj.remove(k);
        }
    }
    fx.write_state("runL", &r);

    // A plain operate must not choke on the missing fields.
    fx.set_status("runL", "t1", "running");

    // And timings must load and render every (still-unmeasured) phase.
    let out = fx.condukt(&["state", "timings", "--run", "runL"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "legacy run-state lacking the phase fields must still load in timings: {out:?}"
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("unmeasured"),
        "legacy record renders phases as unmeasured (got: {text:?})"
    );
}
