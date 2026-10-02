// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 7ca0336b: the `state abandon --task` status guard ("only
//! running/failed tasks can be abandoned") lives inline in main.rs, so the
//! unit test `state::tests::state_abandon_guard_rejects_non_running_non_failed`
//! re-implements the predicate in its own body and would stay green if the
//! main.rs guard were deleted. This binds the SHIPPED binary instead: an
//! explicit abandon of a task that is neither running nor failed must be
//! refused, and the task must keep its status.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn run_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn condukt(repo: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_condukt"))
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("CLAUDE_CODE_SESSION_ID", "sess-7ca0336b")
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env_remove("CONDUKT_DISABLE")
        .output()
        .expect("spawn condukt")
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
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

fn status_of(home: &Path, run: &str, task: &str) -> String {
    let p =
        find_file(&home.join(".condukt").join("state"), &format!("{run}.json")).expect("run state");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    v["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == task)
        .map(|t| t["status"].as_str().unwrap_or("").to_string())
        .expect("task present")
}

#[test]
fn explicit_abandon_refuses_a_task_that_is_neither_running_nor_failed() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    run_git(&repo, &["init", "-q", "-b", "main"]);
    run_git(&repo, &["config", "user.email", "t@t.t"]);
    run_git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let dec = repo.join("dec.json");
    std::fs::write(
        &dec,
        r#"{"goal":"g","tasks":[
            {"id":"tp","title":"p","touched_files":["p.txt"],"deps":[],"class":"parallel","done_criteria":"d"},
            {"id":"td","title":"d","touched_files":["d.txt"],"deps":[],"class":"parallel","done_criteria":"d"}]}"#,
    )
    .unwrap();
    let out = condukt(
        &repo,
        &home,
        &[
            "state",
            "init",
            "--run",
            "runA",
            "--file",
            dec.to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(0), "state init: {out:?}");
    let out = condukt(
        &repo,
        &home,
        &[
            "state", "set", "--run", "runA", "--task", "td", "--status", "done",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "state set done: {out:?}");

    for (task, status) in [("tp", "pending"), ("td", "done")] {
        assert_eq!(
            status_of(&home, "runA", task),
            status,
            "fixture precondition"
        );
        let out = condukt(
            &repo,
            &home,
            &["state", "abandon", "--run", "runA", "--task", task],
        );
        assert!(
            !out.status.success(),
            "abandoning a {status} task must be refused by the shipped binary; \
             stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            status_of(&home, "runA", task),
            status,
            "a refused abandon must leave the {status} task untouched"
        );
    }
}
