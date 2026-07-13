//! End-to-end coverage for `condukt state reconcile`'s cross-run duplicate
//! hashkey completion detection (§4.6c). Spawns the real binary against an
//! isolated HOME so it exercises the actual CLI, the on-disk run-state JSON,
//! and the exit-code contract:
//!
//! (1) When two different run_ids both drove the SAME hashkey to done/verified
//!     (a task executed twice — the last-resort guard against clock skew /
//!     `--force` double-claim / reap-vs-reclaim races), reconcile must NOT
//!     auto-merge or auto-discard: it prints `{"duplicate_completion":[...]}`
//!     on stdout and exits 2 (escalate = needs human).
//!
//! (2) When there is no cross-run duplicate, reconcile behaves exactly as
//!     before: the existing "branch merged into default → auto-verify" path
//!     still fires and the command exits 0 (regression guard).

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
        base.push(format!("condukt-reconcile-dup-e2e-{pid}-{tag}"));
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

    /// Locate a run-state JSON file (`<run>.json`) somewhere under the isolated
    /// HOME's condukt state tree. The exact project-key subdir is derived
    /// internally by condukt, so we find the file by name rather than
    /// reconstructing that derivation.
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

/// (1) Two runs completed the same hashkey → reconcile escalates (exit 2) and
/// emits the duplicate_completion JSON. Neither run is auto-mutated.
#[test]
fn cross_run_duplicate_completion_escalates_exit_2() {
    let fx = Fixture::new("dup");
    let dec_a = fx.write_decomp("decA.json", "t1", "src/x.rs");
    let dec_b = fx.write_decomp("decB.json", "t1", "src/x.rs");

    fx.condukt(&[
        "state",
        "init",
        "--run",
        "runA",
        "--file",
        dec_a.to_str().unwrap(),
    ]);
    fx.condukt(&[
        "state",
        "init",
        "--run",
        "runB",
        "--file",
        dec_b.to_str().unwrap(),
    ]);

    // Inject the shared hashkey + completion into both run states directly.
    // runA claimed at t=100 and is verified; runB completed the SAME hashkey at
    // t=200 (updated_at, i.e. AFTER runA's claim) → a cross-run duplicate.
    let mut a = fx.read_state("runA");
    a["tasks"][0]["hashkey"] = serde_json::json!("hk-shared");
    a["tasks"][0]["claimed_at"] = serde_json::json!(100);
    a["tasks"][0]["updated_at"] = serde_json::json!(150);
    a["tasks"][0]["status"] = serde_json::json!("verified");
    fx.write_state("runA", &a);

    let mut b = fx.read_state("runB");
    b["tasks"][0]["hashkey"] = serde_json::json!("hk-shared");
    b["tasks"][0]["claimed_at"] = serde_json::json!(120);
    b["tasks"][0]["updated_at"] = serde_json::json!(200);
    b["tasks"][0]["status"] = serde_json::json!("done");
    fx.write_state("runB", &b);

    let out = fx.condukt(&["state", "reconcile", "--run", "runA"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "duplicate completion must escalate with exit 2 (escalate=needs human): {out:?}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let payload: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be JSON");
    let dups = payload["duplicate_completion"]
        .as_array()
        .expect("duplicate_completion array");
    assert_eq!(dups.len(), 1, "exactly one duplicated hashkey: {stdout}");
    assert_eq!(dups[0]["hashkey"], "hk-shared");
    let runs: Vec<&str> = dups[0]["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        runs.contains(&"runA") && runs.contains(&"runB"),
        "both runs must be reported for human choice: {stdout}"
    );

    // Neither run was auto-mutated (still exactly as we wrote them).
    let a_after = fx.read_state("runA");
    assert_eq!(a_after["tasks"][0]["status"], "verified");
    let b_after = fx.read_state("runB");
    assert_eq!(b_after["tasks"][0]["status"], "done");
}

/// (2) Regression: no cross-run duplicate → reconcile runs the existing
/// branch-merged→verified auto-promotion and exits 0.
#[test]
fn no_duplicate_branch_merged_auto_verifies_exit_0() {
    let fx = Fixture::new("regression");
    let dec = fx.write_decomp("dec.json", "t1", "src/y.rs");
    fx.condukt(&[
        "state",
        "init",
        "--run",
        "runC",
        "--file",
        dec.to_str().unwrap(),
    ]);

    // Create a branch, commit on it, and MERGE it into main so reconcile's
    // "branch is an ancestor of default" path fires.
    run_git(&fx.repo, &["checkout", "-q", "-b", "condukt/runC-t1"]);
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
        &["merge", "-q", "--no-ff", "condukt/runC-t1", "-m", "merge"],
    );

    // Point the task at the merged branch. It carries a UNIQUE hashkey no other
    // run shares, so the duplicate check is a no-op and the normal path runs.
    let mut c = fx.read_state("runC");
    c["tasks"][0]["status"] = serde_json::json!("done");
    c["tasks"][0]["branch"] = serde_json::json!("condukt/runC-t1");
    c["tasks"][0]["branch_sha"] = serde_json::json!(sha);
    c["tasks"][0]["hashkey"] = serde_json::json!("hk-unique-c");
    c["tasks"][0]["claimed_at"] = serde_json::json!(100);
    c["tasks"][0]["updated_at"] = serde_json::json!(150);
    fx.write_state("runC", &c);

    let out = fx.condukt(&["state", "reconcile", "--run", "runC"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "no duplicate → normal reconcile exits 0: {out:?}"
    );
    // Existing behavior preserved: the merged-branch task was auto-verified.
    let c_after = fx.read_state("runC");
    assert_eq!(
        c_after["tasks"][0]["status"], "verified",
        "branch-merged task must auto-promote to verified (regression)"
    );
}
