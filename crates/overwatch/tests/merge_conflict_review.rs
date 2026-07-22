// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Integration test for the consensus merge-conflict review surface
//! (design 625aa170 B): an open blocked-merge entry surfaces in
//! `overwatch review-queue --json` as a `[merge-conflict]` High row, and
//! `overwatch resolve-merge-conflict` records a resolution so the entry leaves
//! the OPEN set (no longer surfaced). Store isolation via a temp HOME + temp
//! cwd (same hermetic pattern as `review_queue.rs`); the entry is seeded by
//! writing the JSONL directly to the store path the child resolves (mirrors
//! `canary.rs`).

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);
// The store path is resolved under the process-global $HOME; seeding it in-process
// mutates $HOME, so serialize seed+spawn against any other HOME-mutating test.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn make_sandbox(tag: &str) -> (PathBuf, PathBuf) {
    let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!(
        "overwatch-mc-test-{tag}-{}-{n}",
        std::process::id()
    ));
    let home = base.join("home");
    let work = base.join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    (home, work)
}

fn run_ow(home: &Path, work: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_overwatch"))
        .args(args)
        .env("HOME", home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(work)
        .output()
        .expect("failed to spawn overwatch binary");
    assert!(
        out.status.success(),
        "overwatch {:?} exited non-zero: stderr={}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("overwatch stdout not utf8")
}

#[test]
fn open_merge_conflict_surfaces_then_leaves_open_set_on_resolve() {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (home, work) = make_sandbox("resolve");

    // Seed one OPEN blocked-merge entry via the SAME store path the child uses.
    let prev_home = std::env::var_os("HOME");
    std::env::set_var("HOME", &home);
    let mc_path = overwatch::store::merge_conflicts_path(&work).unwrap();
    std::fs::create_dir_all(mc_path.parent().unwrap()).unwrap();
    let entry = serde_json::json!({
        "conflict_id": "runA/condukt-t2/1000",
        "origin": "merge-conflict",
        "run_id": "runA",
        "branch": "condukt/t2",
        "default_branch": "main",
        "base_ref": "deadbeef",
        "conflicted_files": ["crates/x/src/main.rs"],
        "diff_ours": "our side",
        "diff_theirs": "their side",
        "ts": 1000
    });
    std::fs::write(&mc_path, format!("{}\n", entry)).unwrap();
    match &prev_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }

    // review-queue --json surfaces it as a [merge-conflict] High row.
    let out = run_ow(&home, &work, &["review-queue", "--json"]);
    let rows: Vec<Value> = serde_json::from_str(&out).expect("review-queue json");
    let mc = rows
        .iter()
        .find(|r| r["kind"] == "merge-conflict")
        .expect("a merge-conflict row must be surfaced");
    assert_eq!(mc["severity"], "high");
    assert_eq!(mc["identifier"], "runA/condukt-t2/1000");
    assert!(
        mc["summary"]
            .as_str()
            .unwrap()
            .contains("crates/x/src/main.rs"),
        "summary must name the conflicted file: {}",
        mc["summary"]
    );

    // Resolve it (human picks ours).
    let resolved = run_ow(
        &home,
        &work,
        &[
            "resolve-merge-conflict",
            "--id",
            "runA/condukt-t2/1000",
            "--choose",
            "ours",
        ],
    );
    let rj: Value = serde_json::from_str(&resolved).expect("resolve json");
    assert_eq!(rj["resolved"], true);
    assert_eq!(rj["choice"], "ours");

    // review-queue no longer surfaces it (it left the OPEN set).
    let out2 = run_ow(&home, &work, &["review-queue", "--json"]);
    let rows2: Vec<Value> = serde_json::from_str(&out2).expect("review-queue json 2");
    assert!(
        !rows2.iter().any(|r| r["kind"] == "merge-conflict"),
        "resolved conflict must leave the open set: {out2}"
    );
}
