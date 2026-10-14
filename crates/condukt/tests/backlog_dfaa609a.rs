// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro for backlog dfaa609a: `escalate.rs::load` maps an unparseable
//! `escalations.json` to an empty registry (`unwrap_or_default`), so
//! `condukt escalate list` reports "no open escalations" (exit 0, `[]`) for a
//! store it could not read. "Could not read the registry" is not "nothing is
//! waiting on a human".
//!
//! Driven through the real binary with a sandboxed `$HOME`: one escalation is
//! added by `escalate add`, the store file it wrote is then corrupted, and
//! `escalate list --json` is observed.
//!
//! * RED: corrupt store -> list must NOT exit 0 with an empty array.
//! * control: the intact store lists the open escalation.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

struct Fixture {
    base: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
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

impl Fixture {
    fn new(tag: &str) -> Self {
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-dfaa609a-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t.t"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        Self { base, repo, home }
    }

    fn condukt(&self, args: &[&str]) -> Output {
        Command::new(bin())
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env_remove("CONDUKT_DISABLE")
            .output()
            .expect("spawn condukt")
    }

    fn add_one(&self) -> PathBuf {
        let out = self.condukt(&[
            "escalate",
            "add",
            "--run",
            "r1",
            "--task",
            "t1",
            "--question",
            "which way?",
            "--option",
            "a",
            "--option",
            "b",
        ]);
        assert!(out.status.success(), "escalate add: {out:?}");
        find_file(&self.home, "escalations.json").expect("escalations.json written")
    }

    fn list(&self) -> Output {
        self.condukt(&["escalate", "list", "--run", "r1", "--json"])
    }
}

#[test]
#[ignore = "backlog dfaa609a: RED repro"]
fn corrupt_escalations_store_is_not_reported_as_no_open_escalations() {
    let fx = Fixture::new("red");
    let store = fx.add_one();
    std::fs::write(&store, "{ this is not json").unwrap();
    let out = fx.list();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let reads_as_empty = out.status.success()
        && serde_json::from_str::<serde_json::Value>(stdout.trim())
            .ok()
            .and_then(|v| v.as_array().map(Vec::is_empty))
            .unwrap_or(false);
    assert!(
        !reads_as_empty,
        "an unparseable escalations.json was reported as 'no open escalations' \
         (exit 0, []): stdout={stdout} stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn control_intact_store_lists_the_open_escalation() {
    let fx = Fixture::new("control");
    fx.add_one();
    let out = fx.list();
    assert!(out.status.success(), "list: {out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v.as_array().map(Vec::len), Some(1), "{v}");
}
