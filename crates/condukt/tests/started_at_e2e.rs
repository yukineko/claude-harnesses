//! End-to-end coverage for the `started_at` stamping gap. Spawns the real
//! `condukt` binary against an isolated HOME so it exercises the actual
//! `state set` / `state reconcile` CLI paths and the on-disk run-state JSON.
//!
//! The bug: `TaskState.started_at` was stamped ONLY by the explicit `--status
//! running` transition, so any orchestration path that settles a task without
//! passing through `running` left `started_at` (and therefore the derived
//! duration) `None`. The fix stamps `started_at` at the earliest defensible
//! point — when a worktree/branch is assigned, which provably precedes worker
//! execution — set-once so a later transition never overwrites it. Where a task
//! genuinely has no start information (e.g. reconcile-promoted with no
//! worktree/branch) `started_at` MUST stay `None` (never fabricated as 0).

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
        base.push(format!("condukt-started-at-e2e-{pid}-{tag}"));
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
            // Pin the default branch to this test repo's branch so an ambient
            // CONDUKT_DEFAULT_BRANCH inherited from the caller's shell cannot
            // steer reconcile's branch-merged detection at a foreign ref.
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

/// THE GAP (RED before fix): a task driven to a settled state while a
/// worktree/branch is assigned, but WITHOUT ever passing through `--status
/// running`, must now carry a stamped `started_at` (and thus a measurable
/// duration), instead of being silently durationless.
#[test]
fn assigning_branch_without_running_transition_stamps_started_at() {
    let fx = Fixture::new("gap");
    let dec = fx.write_decomp("dec.json", "t1", "src/a.rs");
    fx.condukt(&[
        "state",
        "init",
        "--run",
        "runS",
        "--file",
        dec.to_str().unwrap(),
    ]);

    // Assign a branch WITHOUT ever taking the `running` edge (the orchestrator
    // path that settles a task without a running transition). This is the
    // earliest defensible start instant, so started_at must be stamped here.
    let out = fx.condukt(&[
        "state",
        "set",
        "--run",
        "runS",
        "--task",
        "t1",
        "--status",
        "pending",
        "--branch",
        "condukt/runS-t1",
    ]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "state set (assign branch) must succeed: {out:?}"
    );
    let after_assign = fx.read_state("runS");
    let started = after_assign["tasks"][0]["started_at"].as_i64();
    assert!(
        started.is_some(),
        "started_at must be stamped at branch assignment even without a running transition (got {:?})",
        after_assign["tasks"][0]["started_at"]
    );

    // Later, settle straight to `done` (still no `running`). updated_at is now
    // set and must be >= the earlier start, yielding a measurable duration.
    let out = fx.condukt(&[
        "state", "set", "--run", "runS", "--task", "t1", "--status", "done",
    ]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "state set (settle) must succeed: {out:?}"
    );

    let s = fx.read_state("runS");
    let started2 = s["tasks"][0]["started_at"].as_i64();
    let updated = s["tasks"][0]["updated_at"].as_i64();
    assert_eq!(
        started2, started,
        "started_at must be set-once: the settle transition must not move it"
    );
    assert!(updated.is_some(), "updated_at must be set on settle");
    assert!(
        started2.unwrap() <= updated.unwrap(),
        "the earlier start must not be after the settle time: started={started2:?} updated={updated:?}"
    );
}

/// REQUIREMENT: a path with genuinely NO start information must keep
/// `started_at` = None (so the derived duration is None, never a fabricated 0).
/// A reconcile-promoted merged-branch task that never recorded a start is that
/// path — reconcile must NOT invent a start instant.
#[test]
fn settled_via_reconcile_without_start_info_keeps_started_at_none() {
    let fx = Fixture::new("noinfo");
    let dec = fx.write_decomp("dec.json", "t1", "src/y.rs");
    fx.condukt(&[
        "state",
        "init",
        "--run",
        "runR",
        "--file",
        dec.to_str().unwrap(),
    ]);

    // Create + merge a branch so reconcile's "branch merged → auto-verify" path
    // fires.
    run_git(&fx.repo, &["checkout", "-q", "-b", "condukt/runR-t1"]);
    std::fs::write(fx.repo.join("src-y.txt"), "work\n").unwrap();
    run_git(&fx.repo, &["add", "."]);
    run_git(&fx.repo, &["commit", "-q", "-m", "task work"]);
    let sha = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&fx.repo)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    run_git(&fx.repo, &["checkout", "-q", "main"]);
    run_git(
        &fx.repo,
        &["merge", "-q", "--no-ff", "condukt/runR-t1", "-m", "merge"],
    );

    // Inject a done task pointing at the merged branch, with NO started_at.
    let mut r = fx.read_state("runR");
    r["tasks"][0]["status"] = serde_json::json!("done");
    r["tasks"][0]["branch"] = serde_json::json!("condukt/runR-t1");
    r["tasks"][0]["branch_sha"] = serde_json::json!(sha);
    r["tasks"][0]["updated_at"] = serde_json::json!(150);
    // Ensure the field is genuinely absent.
    if let Some(obj) = r["tasks"][0].as_object_mut() {
        obj.remove("started_at");
    }
    fx.write_state("runR", &r);

    let out = fx.condukt(&["state", "reconcile", "--run", "runR"]);
    assert_eq!(out.status.code(), Some(0), "reconcile exits 0: {out:?}");

    let after = fx.read_state("runR");
    assert_eq!(
        after["tasks"][0]["status"], "verified",
        "merged branch must auto-promote (regression)"
    );
    assert!(
        after["tasks"][0]["started_at"].is_null(),
        "reconcile must NOT fabricate a start; started_at stays None (got {:?})",
        after["tasks"][0]["started_at"]
    );
}

/// REQUIREMENT: set-once. A later `running` transition must NOT overwrite an
/// already-recorded `started_at` (which would shrink a real duration).
#[test]
fn started_at_is_not_overwritten_on_a_later_running_transition() {
    let fx = Fixture::new("noverwrite");
    let dec = fx.write_decomp("dec.json", "t1", "src/z.rs");
    fx.condukt(&[
        "state",
        "init",
        "--run",
        "runO",
        "--file",
        dec.to_str().unwrap(),
    ]);

    // Pre-seed a start far in the past (deterministic sentinel).
    let mut r = fx.read_state("runO");
    r["tasks"][0]["started_at"] = serde_json::json!(42);
    fx.write_state("runO", &r);

    let out = fx.condukt(&[
        "state", "set", "--run", "runO", "--task", "t1", "--status", "running",
    ]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "state set must succeed: {out:?}"
    );

    let after = fx.read_state("runO");
    assert_eq!(
        after["tasks"][0]["started_at"].as_i64(),
        Some(42),
        "a later running transition must not overwrite the recorded started_at"
    );
}

/// BACK-COMPAT: run-state JSON written before `started_at` existed (field
/// absent) still deserializes and can be operated on.
#[test]
fn legacy_run_state_without_started_at_field_still_reads() {
    let fx = Fixture::new("legacy");
    let dec = fx.write_decomp("dec.json", "t1", "src/w.rs");
    fx.condukt(&[
        "state",
        "init",
        "--run",
        "runL",
        "--file",
        dec.to_str().unwrap(),
    ]);

    // Strip started_at (and updated_at) to emulate a pre-field on-disk record.
    let mut r = fx.read_state("runL");
    if let Some(obj) = r["tasks"][0].as_object_mut() {
        obj.remove("started_at");
        obj.remove("updated_at");
    }
    fx.write_state("runL", &r);

    // A plain read/operate must not choke on the missing field.
    let out = fx.condukt(&[
        "state", "set", "--run", "runL", "--task", "t1", "--status", "done",
    ]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "legacy run-state lacking started_at must still load and operate: {out:?}"
    );
}
