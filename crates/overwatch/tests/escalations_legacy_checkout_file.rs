#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The LEGACY half of `review_escalation::condukt_escalations_paths`: besides
//! the main-worktree-keyed `escalations.json` every checkout shares, a linked
//! worktree also reads the per-checkout file keyed on
//! `projkey::repo_root(cwd)` (where a pre-re-keying condukt wrote from that
//! worktree), and `scan_open_escalations` merges both, canonical first, deduped
//! by id. `escalations_shared_across_worktrees.rs` only covers the canonical
//! file; this file covers the legacy read, the cross-file dedupe, and that a
//! corrupt legacy file is never reported as a clean (empty) queue.
//!
//! Drives the real `overwatch review-queue --json` binary with HOME pinned to a
//! tempdir (condukt's default state dir is `$HOME/.condukt/state`).

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const LEGACY_ID: &str = "esc-legacy-only-1";
const SHARED_ID: &str = "esc-in-both-1";
const CANON_ID: &str = "esc-canonical-1";

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
        wt,
    }
}

fn state_file(home: &Path, root: &Path) -> PathBuf {
    home.join(".condukt")
        .join("state")
        .join(harness_core::projkey::project_key(root))
        .join("escalations.json")
}

/// Canonical file: keyed on `main_worktree_root(cwd)`.
fn canonical_file(home: &Path, cwd: &Path) -> PathBuf {
    let root = match harness_core::projkey::main_worktree_root(cwd) {
        harness_core::verdict::Determination::Known(r) => r,
        other => panic!("precondition: main worktree root must be determinable: {other:?}"),
    };
    state_file(home, &root)
}

/// Legacy per-checkout file: keyed on `repo_root(cwd)` (the linked worktree
/// itself). Asserted distinct from the canonical one, or the fixture tests
/// nothing about the legacy read.
fn legacy_file(home: &Path, cwd: &Path) -> PathBuf {
    let legacy = state_file(home, &harness_core::projkey::repo_root(cwd));
    assert_ne!(
        legacy,
        canonical_file(home, cwd),
        "precondition: from a linked worktree the legacy per-checkout file must differ \
         from the canonical main-worktree-keyed file"
    );
    legacy
}

fn escalation_json(id: &str, question: &str) -> String {
    format!(
        r#"{{"id":"{id}","run":"r","task":"t","question":"{question}",
        "options":["a","b"],"recommended":0,"created_at":500,"resolved":false}}"#
    )
}

fn write_registry(file: &Path, rows: &[String]) {
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, format!(r#"{{"escalations":[{}]}}"#, rows.join(","))).unwrap();
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

fn rows(out: &Output) -> Vec<Value> {
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "review-queue stdout is not JSON ({e}): status={:?} stdout={} stderr={}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    v.as_array().expect("json array").clone()
}

fn escalation_ids(out: &Output) -> Vec<String> {
    assert!(
        out.status.success(),
        "review-queue failed: status={:?} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    rows(out)
        .iter()
        .filter(|r| r["kind"] == "escalation")
        .map(|r| r["identifier"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn open_escalation_only_in_legacy_checkout_file_is_listed_from_linked_worktree() {
    let fx = fixture();
    let legacy = legacy_file(&fx.home, &fx.wt);
    write_registry(&legacy, &[escalation_json(LEGACY_ID, "legacy Q?")]);
    assert!(
        !canonical_file(&fx.home, &fx.wt).exists(),
        "precondition: the escalation lives ONLY in the legacy file"
    );

    let out = review_queue(&fx.home, &fx.wt);
    let ids = escalation_ids(&out);
    assert!(
        ids.iter().any(|i| i == LEGACY_ID),
        "an open escalation present only in the linked worktree's legacy per-checkout \
         escalations.json must be listed (condukt still reads that file); saw {ids:?}; \
         stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn same_escalation_id_in_canonical_and_legacy_file_is_listed_once() {
    let fx = fixture();
    write_registry(
        &canonical_file(&fx.home, &fx.wt),
        &[escalation_json(SHARED_ID, "canonical copy?")],
    );
    write_registry(
        &legacy_file(&fx.home, &fx.wt),
        &[escalation_json(SHARED_ID, "legacy copy?")],
    );

    let out = review_queue(&fx.home, &fx.wt);
    let ids = escalation_ids(&out);
    let n = ids.iter().filter(|i| *i == SHARED_ID).count();
    assert_eq!(
        n, 1,
        "the same escalation id in both the canonical and the legacy file must be listed \
         exactly once (dedupe by id across files); saw {ids:?}"
    );
}

#[test]
fn corrupt_legacy_file_is_never_reported_as_a_clean_queue() {
    let fx = fixture();
    // A valid canonical file, so the only defect is the legacy one.
    write_registry(
        &canonical_file(&fx.home, &fx.wt),
        &[escalation_json(CANON_ID, "canonical Q?")],
    );
    let legacy = legacy_file(&fx.home, &fx.wt);
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "{ this is not a registry").unwrap();

    let out = review_queue(&fx.home, &fx.wt);
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let refused = !out.status.success();
    let announced = stderr.to_lowercase().contains("escalation");
    assert!(
        refused || announced,
        "the linked worktree's legacy escalations.json does not parse, yet review-queue \
         succeeded and said nothing about the escalation source (a corrupt queue reported \
         as clean). status={:?} stderr={stderr:?} stdout={}",
        out.status,
        String::from_utf8_lossy(&out.stdout)
    );
    // And never a silent empty list: if the command still emits rows, the
    // escalation source must be marked undetermined in-band.
    if !out.stdout.is_empty() {
        let rs = rows(&out);
        let marked = rs
            .iter()
            .any(|r| r["kind"] == "undetermined-source" && r["identifier"] == "escalations.json");
        assert!(
            marked,
            "review-queue emitted rows without an in-band undetermined marker for the \
             escalation source while the legacy file is corrupt: {rs:?}"
        );
    }
}
