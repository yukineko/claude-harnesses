//! End-to-end coverage for the cross-session file-claim registry — the PDO
//! (Parallel Development Orchestration) collision guard. Spawns the real binary
//! against an isolated HOME so it exercises the actual CLI, the on-disk
//! `claims.json`, and — critically — the AUTO-claim/skip/release wiring baked
//! into `state set --status running`, which the in-module unit tests cannot
//! reach. Two runs in the same project fight over the same `touched_files`:
//! the first wins, the second is hard-skipped, and once the first settles the
//! second can proceed.

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
        base.push(format!("condukt-claim-e2e-{pid}-{tag}"));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["config", "user.email", "t@t.t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        Self { repo, home }
    }

    fn condukt(&self, args: &[&str]) -> Output {
        Command::new(bin())
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            // A stable session id so claims are attributable; the real driver
            // passes the Claude session's id here.
            .env("CLAUDE_CODE_SESSION_ID", "sess-test")
            .output()
            .expect("spawn condukt")
    }

    fn write_decomp(&self, name: &str, file: &str) -> PathBuf {
        let p = self.repo.join(name);
        let json = format!(
            r#"{{"goal":"touch {file}","tasks":[{{"id":"t1","title":"edit {file}","touched_files":["{file}"],"deps":[],"class":"parallel","done_criteria":"d"}}]}}"#
        );
        std::fs::write(&p, json).unwrap();
        p
    }
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

#[test]
fn running_a_task_claims_and_a_conflicting_run_is_hard_skipped_then_released() {
    let fx = Fixture::new("hard-skip");
    let dec_a = fx.write_decomp("decA.json", "src/shared.rs");
    let dec_b = fx.write_decomp("decB.json", "src/shared.rs");

    // Two independent runs (different sessions would produce these) that both
    // want to touch src/shared.rs.
    let init_a = fx.condukt(&[
        "state",
        "init",
        "--run",
        "runA",
        "--file",
        dec_a.to_str().unwrap(),
    ]);
    assert!(init_a.status.success(), "init A failed: {init_a:?}");
    let init_b = fx.condukt(&[
        "state",
        "init",
        "--run",
        "runB",
        "--file",
        dec_b.to_str().unwrap(),
    ]);
    assert!(init_b.status.success(), "init B failed: {init_b:?}");

    // A marks its task running → auto-claims src/shared.rs.
    let a_run = fx.condukt(&[
        "state", "set", "--run", "runA", "--task", "t1", "--status", "running",
    ]);
    assert!(
        a_run.status.success(),
        "A running should succeed: {a_run:?}"
    );

    // The registry now shows A holding the file.
    let claims = fx.condukt(&["state", "claims"]);
    let claims_out = String::from_utf8_lossy(&claims.stdout);
    assert!(
        claims_out.contains("src/shared.rs") && claims_out.contains("runA"),
        "claims should show runA holding src/shared.rs, got: {claims_out}"
    );

    // B tries to run the SAME file → HARD SKIP: exit non-zero + skip JSON that
    // names the live holder.
    let b_run = fx.condukt(&[
        "state", "set", "--run", "runB", "--task", "t1", "--status", "running",
    ]);
    assert!(
        !b_run.status.success(),
        "B running should be hard-skipped (non-zero exit): {b_run:?}"
    );
    let b_out = String::from_utf8_lossy(&b_run.stdout);
    assert!(
        b_out.contains("\"skipped\": true") && b_out.contains("runA"),
        "B's skip JSON should report the live holder runA, got: {b_out}"
    );

    // A finishes (terminal) → its files auto-release.
    let a_fail = fx.condukt(&[
        "state", "set", "--run", "runA", "--task", "t1", "--status", "failed",
    ]);
    assert!(a_fail.status.success(), "A failed transition: {a_fail:?}");

    let claims2 = fx.condukt(&["state", "claims"]);
    let claims2_out = String::from_utf8_lossy(&claims2.stdout);
    assert!(
        !claims2_out.contains("src/shared.rs"),
        "src/shared.rs should be released after A settled, got: {claims2_out}"
    );

    // B can now claim it.
    let b_run2 = fx.condukt(&[
        "state", "set", "--run", "runB", "--task", "t1", "--status", "running",
    ]);
    assert!(
        b_run2.status.success(),
        "B should now succeed after release: {b_run2:?}"
    );
}

#[test]
fn non_conflicting_runs_do_not_block_each_other() {
    let fx = Fixture::new("disjoint");
    let dec_a = fx.write_decomp("decA.json", "src/a.rs");
    let dec_b = fx.write_decomp("decB.json", "src/b.rs");
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

    let a = fx.condukt(&[
        "state", "set", "--run", "runA", "--task", "t1", "--status", "running",
    ]);
    let b = fx.condukt(&[
        "state", "set", "--run", "runB", "--task", "t1", "--status", "running",
    ]);
    assert!(a.status.success(), "A (src/a.rs) should run: {a:?}");
    assert!(
        b.status.success(),
        "B (src/b.rs) touches a disjoint file — must not be skipped: {b:?}"
    );
}

#[test]
fn explicit_release_frees_a_claim_for_another_run() {
    let fx = Fixture::new("explicit-release");
    let dec_a = fx.write_decomp("decA.json", "src/shared.rs");
    fx.condukt(&[
        "state",
        "init",
        "--run",
        "runA",
        "--file",
        dec_a.to_str().unwrap(),
    ]);
    fx.condukt(&[
        "state", "set", "--run", "runA", "--task", "t1", "--status", "running",
    ]);

    // Explicit release (as the driver would on cleanup) frees the file.
    let rel = fx.condukt(&["state", "release", "--run", "runA"]);
    assert!(rel.status.success(), "release failed: {rel:?}");

    let claims = fx.condukt(&["state", "claims"]);
    assert!(
        !String::from_utf8_lossy(&claims.stdout).contains("src/shared.rs"),
        "release should have emptied the registry"
    );
}
