#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! DoD-14: condukt writes `escalations.json` under
//! `<state_dir>/<project_key(main_worktree_root(cwd))>/` (condukt
//! `escalate.rs::escalations_path` -> `state::project_state_dir`), so EVERY
//! checkout of a repo shares one queue. overwatch's reader
//! (`review_escalation::condukt_escalations_path`) still derives the key from
//! `projkey::repo_root(cwd)` (per checkout), so from a linked worktree it reads a
//! different directory and reports a real, open human question as an empty queue.
//!
//! `review_escalation` is a bin-only module, so these tests drive the real
//! `overwatch review-queue --json` binary with a sandboxed HOME (condukt's
//! default state dir is `$HOME/.condukt/state`, the same default overwatch
//! mirrors). The escalation is placed where condukt would write it, derived with
//! the same harness_core functions condukt's `state::project_dir` calls:
//! `main_worktree_root` then `project_key`.
//!
//! RED by design: `open_escalation_is_visible_from_main_and_from_linked_worktree`
//! fails on the current code. The two controls pass today.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ESC_ID: &str = "esc-dod14-1";

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

struct Fixture {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    main: PathBuf,
    wt: PathBuf,
}

fn fixture() -> Fixture {
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
    Fixture {
        _tmp: tmp,
        home,
        main,
        wt,
    }
}

/// Where condukt writes `escalations.json` for the project `cwd` belongs to:
/// `<HOME>/.condukt/state/<project_key(main_worktree_root(cwd))>/escalations.json`.
fn condukt_escalations_file(home: &Path, cwd: &Path) -> PathBuf {
    let root = match harness_core::projkey::main_worktree_root(cwd) {
        harness_core::verdict::Determination::Known(r) => r,
        other => panic!("precondition: main worktree root must be determinable: {other:?}"),
    };
    home.join(".condukt")
        .join("state")
        .join(harness_core::projkey::project_key(&root))
        .join("escalations.json")
}

fn seed_open_escalation(file: &Path) {
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        file,
        format!(
            r#"{{"escalations":[{{"id":"{ESC_ID}","run":"r","task":"t","question":"Q?",
            "options":["a","b"],"recommended":0,"created_at":500,"resolved":false}}]}}"#
        ),
    )
    .unwrap();
}

fn review_queue(home: &Path, cwd: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_overwatch"))
        .args(["review-queue", "--json"])
        .env("HOME", home)
        .env_remove("HARNESS_SANDBOX_HOME")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(cwd)
        .output()
        .expect("spawn overwatch")
}

fn escalation_ids(out: &Output) -> Vec<String> {
    assert!(
        out.status.success(),
        "review-queue failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rows: Value = serde_json::from_slice(&out.stdout).expect("json");
    rows.as_array()
        .unwrap()
        .iter()
        .filter(|r| r["kind"] == "escalation")
        .map(|r| r["identifier"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn open_escalation_is_visible_from_main_and_from_linked_worktree() {
    let fx = fixture();
    // Placed exactly where condukt writes it (main-worktree keyed). Both
    // checkouts must resolve to this one file.
    let from_main_key = condukt_escalations_file(&fx.home, &fx.main);
    let from_wt_key = condukt_escalations_file(&fx.home, &fx.wt);
    assert_eq!(
        from_main_key, from_wt_key,
        "precondition: condukt's keying is shared by every checkout of the repo"
    );
    seed_open_escalation(&from_main_key);

    let main_out = review_queue(&fx.home, &fx.main);
    let main_ids = escalation_ids(&main_out);
    assert!(
        main_ids.iter().any(|i| i == ESC_ID),
        "main checkout must see the open escalation; saw {main_ids:?}"
    );

    let wt_out = review_queue(&fx.home, &fx.wt);
    let wt_ids = escalation_ids(&wt_out);
    assert!(
        wt_ids.iter().any(|i| i == ESC_ID),
        "a linked worktree must see the same open escalation condukt wrote under the \
         main-worktree key (overwatch keys on repo_root, so it reads an empty store); \
         saw {wt_ids:?}; stderr: {}",
        String::from_utf8_lossy(&wt_out.stderr)
    );
}

#[test]
fn never_written_store_reads_as_known_empty_from_both_checkouts() {
    let fx = fixture();
    for (label, cwd) in [("main", &fx.main), ("linked worktree", &fx.wt)] {
        let out = review_queue(&fx.home, cwd);
        let ids = escalation_ids(&out);
        assert!(ids.is_empty(), "{label}: no store, no escalations: {ids:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.to_lowercase().contains("escalation"),
            "{label}: an absent store is a real zero, not an undetermined source, so no \
             escalation warning is expected: {stderr}"
        );
    }
}

#[test]
fn undeterminable_main_worktree_root_is_not_reported_as_known_empty() {
    let fx = fixture();
    // A `.git` FILE with no `gitdir:` line: the checkout looks like a linked
    // worktree but its main worktree root cannot be established
    // (`main-worktree-root: gitfile-unparseable`).
    let broken = fx.home.join("broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join(".git"), "garbage, not a gitdir pointer\n").unwrap();
    assert!(
        matches!(
            harness_core::projkey::main_worktree_root(&broken),
            harness_core::verdict::Determination::Undetermined(_)
        ),
        "precondition: main_worktree_root must be Undetermined for this fixture"
    );

    let out = review_queue(&fx.home, &broken);
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    // Restrictive resolution: either the command refuses (non-zero) or it
    // announces that the escalation source could not be read. Silent success
    // with an empty escalation list is the fail-open.
    let refused = !out.status.success();
    let announced = stderr.to_lowercase().contains("escalation");
    assert!(
        refused || announced,
        "main worktree root undeterminable, yet review-queue succeeded and said nothing \
         about the escalation source (a known-empty queue reported for an unknown store). \
         status={:?} stderr={stderr:?} stdout={}",
        out.status,
        String::from_utf8_lossy(&out.stdout)
    );
}
