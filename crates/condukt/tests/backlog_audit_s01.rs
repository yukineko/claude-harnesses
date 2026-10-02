// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Audit repro tests for open condukt backlog items (shard s01-condukt).
//!
//! Each `#[ignore]`d test pins an OBSERVED open defect: it was run RED against
//! the binary before being ignored. Remove the `#[ignore]` when the defect is
//! fixed. The one non-ignored test is a control that must stay green.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

struct Fx {
    base: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(o.status.success(), "git {args:?}: {o:?}");
}

impl Fx {
    fn new(tag: &str) -> Self {
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-audit-s01-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t.t"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        Self { base, repo, home }
    }

    fn run_in(&self, dir: &Path, args: &[&str]) -> Output {
        Command::new(bin())
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.home)
            .env("CONDUKT_WORKTREE_BASE", self.base.join("wtb"))
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .output()
            .expect("spawn condukt")
    }

    fn c(&self, args: &[&str]) -> Output {
        self.run_in(&self.repo, args)
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// backlog 55f55932 / 65d0c726: an empty hashkey must be refused, not recorded.
#[test]
#[ignore = "backlog 55f55932: open defect, remove ignore when fixed"]
fn claim_task_rejects_empty_hashkey() {
    let f = Fx::new("emptykey");
    let o = f.c(&["state", "claim-task", "--run", "r1", "--hashkey", ""]);
    assert!(
        !o.status.success(),
        "claim-task with an empty hashkey must fail, got rc=0: {}",
        text(&o)
    );
}

/// backlog 752e006a / c10bbb8e: a run-qualified --branch must not be prefixed twice.
#[test]
#[ignore = "backlog 752e006a: open defect, remove ignore when fixed"]
fn worktree_create_does_not_double_the_run_prefix() {
    let f = Fx::new("dblprefix");
    let o = f.c(&[
        "worktree",
        "create",
        "--run",
        "run-X",
        "--topic",
        "t3",
        "--branch",
        "condukt/run-X/t3",
    ]);
    assert!(o.status.success(), "create failed: {}", text(&o));
    let br = Command::new("git")
        .args(["branch", "--list"])
        .current_dir(&f.repo)
        .output()
        .unwrap();
    let br = String::from_utf8_lossy(&br.stdout).to_string();
    assert!(
        !br.contains("run-X/run-X"),
        "branch name carries the run id twice: {br}"
    );
}

/// backlog 4708069b / 43393ce2: run-state created in the main checkout must be
/// visible from a linked worktree of the same repository.
#[test]
#[ignore = "backlog 4708069b: open defect, remove ignore when fixed"]
fn run_state_is_visible_from_a_linked_worktree() {
    let f = Fx::new("statesplit");
    let d = f.base.join("d.json");
    std::fs::write(
        &d,
        r#"{"goal":"g","tasks":[{"id":"t1","title":"x","touched_files":["a.txt"],"deps":[],"class":"parallel","done_criteria":"d"}]}"#,
    )
    .unwrap();
    let o = f.c(&[
        "state",
        "init",
        "--run",
        "run-A",
        "--file",
        d.to_str().unwrap(),
    ]);
    assert!(o.status.success(), "init: {}", text(&o));
    let wt = f.base.join("wt");
    git(
        &f.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "wtb",
            wt.to_str().unwrap(),
            "HEAD",
        ],
    );
    let o = f.run_in(&wt, &["state", "show", "--run", "run-A"]);
    assert!(
        o.status.success(),
        "run-A is invisible from a linked worktree of the same repo: {}",
        text(&o)
    );
}

/// backlog 3487df07: a session id containing a path separator must be rejected.
#[test]
#[ignore = "backlog 3487df07: open defect, remove ignore when fixed"]
fn circuit_check_rejects_session_id_with_path_separator() {
    let f = Fx::new("sessid");
    let o = f.c(&["circuit", "check", "--run", "flow-x", "--session", "a/b"]);
    let t = text(&o);
    assert!(
        t.contains("invalid") || t.contains("reject") || o.status.code() == Some(2),
        "a session id with '/' was accepted and evaluated as if valid (rc={:?}): {t}",
        o.status.code()
    );
}

/// Control for backlog fe20606f: five task claims then a heartbeat keeps the
/// claim visible. This PASSES today; the reported vanishing was NOT reproduced.
#[test]
fn control_task_claims_survive_a_heartbeat() {
    let f = Fx::new("hb");
    for hk in [
        "00d1160bf294b1af",
        "052a7668cb486c1f",
        "9e513f0365cf89ba",
        "ea7d776df3b89745",
        "1d8df0e8d35d6398",
    ] {
        let o = f.c(&["state", "claim-task", "--run", "flow-s1", "--hashkey", hk]);
        assert!(o.status.success(), "{}", text(&o));
    }
    let o = f.c(&["state", "heartbeat", "--run", "flow-s1"]);
    assert!(o.status.success(), "{}", text(&o));
    let o = f.c(&["state", "is-claimed", "--hashkey", "00d1160bf294b1af"]);
    assert!(text(&o).contains("\"claimed\": true"), "{}", text(&o));
}
