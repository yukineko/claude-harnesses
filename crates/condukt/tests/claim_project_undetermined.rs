//! Behavioural coverage for `claim::claim_tasks`' fail-CLOSED arm when the claim
//! registry's PROJECT ROOT cannot be resolved.
//!
//! # Why this file exists
//!
//! The claim registry is keyed by `harness_core::projkey::main_worktree_root`,
//! which is a three-valued answer: it can say "I cannot determine which repo
//! this is". When it does, condukt does not know WHICH `claims.json` this repo's
//! sessions share, so it cannot tell whether the work is already claimed by a
//! peer session. Falling back to a per-worktree registry there would answer
//! "free" for work another session already holds — the exact double-work
//! fail-open backlog `d83b0e8f` closed (CLAUDE.md §3: cannot-determine resolves
//! to the restrictive side, never to `clean`).
//!
//! The mutation battery run when that arm landed reported it as SURVIVING:
//! replacing the hard-skip with the old `repo_root` fallback broke no test. So
//! the arm is exercised here end-to-end against the REAL binary (condukt has no
//! `lib.rs`, so a black-box CLI drive is the only way in), modelled on
//! `tests/claim_registry_e2e.rs`.
//!
//! The observable contract, read off `main.rs`'s `StateAction::ClaimTask` arm:
//! the outcome JSON goes to stdout, and a non-empty `skipped` list exits **1**.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

/// Fresh per-pid temp base; removed first so a previous aborted run cannot leak
/// state into this one. Never touches the real `~/.condukt`: `HOME` is
/// redirected at every invocation.
fn tmp_base(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!(
        "condukt-claim-projundet-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    base
}

fn claim(cwd: &Path, home: &Path, run: &str, hashkey: &str) -> Output {
    Command::new(bin())
        .args(["state", "claim-task", "--run", run, "--hashkey", hashkey])
        .current_dir(cwd)
        .env("HOME", home)
        .env("CLAUDE_CODE_SESSION_ID", "sess-projundet")
        .output()
        .expect("spawn condukt")
}

fn json_of(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout was not valid claim-outcome JSON ({e}): stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    })
}

fn run_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A directory whose `.git` is a FILE with no `gitdir:` line. `repo_root` stops
/// there (`.git` exists), then `main_worktree_root` cannot tell which git dir it
/// designates -> `Undetermined("main-worktree-root: gitfile-unparseable: ...")`.
#[test]
fn unresolvable_project_root_hard_skips_with_project_undetermined_holder() {
    let base = tmp_base("undet");
    let home = base.join("home");
    let broken = base.join("broken-repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&broken).unwrap();
    // Garbage in place of a gitdir pointer.
    std::fs::write(broken.join(".git"), "this is not a gitdir pointer\n").unwrap();

    let out = claim(&broken, &home, "runX", "H-undet");
    let v = json_of(&out);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    assert_eq!(
        v["claimed"],
        serde_json::json!([]),
        "a hashkey must NOT be reported claimed when the project root is \
         undetermined — that is the double-work fail-open. Got: {v} stderr={stderr}"
    );

    let skipped = v["skipped"]
        .as_array()
        .unwrap_or_else(|| panic!("outcome JSON has no `skipped` array: {v}"));
    assert_eq!(
        skipped.len(),
        1,
        "the one requested hashkey must be hard-skipped; got: {v} stderr={stderr}"
    );
    assert_eq!(
        skipped[0]["file"], "H-undet",
        "the skip entry must name the requested hashkey; got: {v}"
    );
    assert_eq!(
        skipped[0]["holder_run"],
        serde_json::json!("__project_undetermined__"),
        "the skip must be stamped with the PROJECT-UNDETERMINED marker, which is \
         deliberately distinct from the lock-contention marker \
         `__lock_contended__`: an operator reads the skip JSON to learn WHICH \
         refusal this was. Got: {v} stderr={stderr}"
    );

    assert_eq!(
        out.status.code(),
        Some(1),
        "a hard skip must exit 1 (main.rs's StateAction::ClaimTask exits 1 when \
         `skipped` is non-empty); got {:?} with stdout={v} stderr={stderr}",
        out.status.code()
    );

    // The refusal must not be silent (CLAUDE.md §4): stderr says WHY the
    // resolution failed, stdout says WHICH refusal it was.
    assert!(
        stderr.contains("main-worktree-root:"),
        "the undetermined reason must be surfaced on stderr, not swallowed; \
         got stderr={stderr}"
    );
}

/// Anti-vacuity control: without it this test would also pass if claiming were
/// broken everywhere (every claim skipping for any reason). The SAME command in
/// a well-formed repo must CLAIM (exit 0, empty `skipped`).
#[test]
fn control_wellformed_repo_claims_successfully() {
    let base = tmp_base("control");
    let home = base.join("home");
    let repo = base.join("good-repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["config", "user.email", "t@t.t"]);
    run_git(&repo, &["config", "user.name", "t"]);

    let out = claim(&repo, &home, "runX", "H-undet");
    let v = json_of(&out);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    assert_eq!(
        v["claimed"],
        serde_json::json!(["H-undet"]),
        "control: a well-formed repo must claim the hashkey; got {v} stderr={stderr}"
    );
    assert_eq!(
        v["skipped"],
        serde_json::json!([]),
        "control: nothing should be skipped; got {v} stderr={stderr}"
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "control: a successful claim must exit 0; got {:?} stdout={v} stderr={stderr}",
        out.status.code()
    );
}
