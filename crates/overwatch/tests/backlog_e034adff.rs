#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog e034adff: overwatch's `condukt_escalations_path` (and condukt's own
//! `escalate.rs::escalations_path`) key the escalation registry on
//! `projkey::repo_root(cwd)` — per CHECKOUT — while the rest of the overwatch
//! store keys on `projkey::main_worktree_root`. So an escalation condukt
//! raised from a linked worktree (the §8-mandated place to work) lands under
//! the worktree's key, and `overwatch review-queue` run from the main checkout
//! never sees it.
//!
//! Repro: a real repo + linked worktree in a temp dir, a sandboxed HOME, the
//! escalation seeded exactly where condukt would write it from the worktree,
//! and the REAL `overwatch review-queue --json` run from main.

use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "backlog e034adff: open defect, remove ignore when fixed"]
fn escalation_raised_in_a_worktree_is_visible_from_main_review_queue() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let home = base.join("home");
    let main = base.join("main");
    let wt = base.join("wt");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&main).unwrap();
    git(&main, &["init", "-q", "-b", "main"]);
    std::fs::write(main.join("a.txt"), "a\n").unwrap();
    git(&main, &["add", "a.txt"]);
    git(&main, &["commit", "-q", "-m", "init"]);
    git(
        &main,
        &["worktree", "add", "-q", "-b", "feat", wt.to_str().unwrap()],
    );

    // Where condukt (escalate.rs: state_dir/<project_key(repo_root(cwd))>)
    // writes when it runs inside the worktree.
    let wt_root = harness_core::projkey::repo_root(&wt);
    let main_root = harness_core::projkey::repo_root(&main);
    assert_ne!(
        harness_core::projkey::project_key(&wt_root),
        harness_core::projkey::project_key(&main_root),
        "precondition: per-checkout keys differ between main and its worktree"
    );
    let esc = home
        .join(".condukt")
        .join("state")
        .join(harness_core::projkey::project_key(&wt_root))
        .join("escalations.json");
    std::fs::create_dir_all(esc.parent().unwrap()).unwrap();
    std::fs::write(
        &esc,
        r#"{"escalations":[{"id":"esc-wt-1","run":"r","task":"t","question":"Q from worktree?",
            "options":["a","b"],"recommended":0,"created_at":500,"resolved":false}]}"#,
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_overwatch"))
        .args(["review-queue", "--json"])
        .env("HOME", &home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(&main)
        .output()
        .expect("spawn overwatch");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "review-queue failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rows: Value = serde_json::from_str(&stdout).expect("json");
    let ids: Vec<&Value> = rows
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["kind"] == "escalation")
        .map(|r| &r["identifier"])
        .collect();
    assert!(
        ids.iter().any(|v| *v == "esc-wt-1"),
        "an open escalation raised from a linked worktree is invisible to the main \
         checkout's review-queue (keyed per checkout, not on the main worktree): {stdout}"
    );
}
