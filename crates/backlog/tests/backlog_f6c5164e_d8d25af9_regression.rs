//! Shared-failure pin for backlog f6c5164e (closed as DUPLICATE) and d8d25af9.
//!
//! Both tickets: `backlog` has no permanent "will not do" close. `done` claims
//! completion, `fail` always sets `defer_until` (+2 days) so the item is
//! re-queued, and `edit --status cancelled` is refused because the CLI only
//! accepts pending|done|failed.
//!
//! `failed_is_always_requeued` is a non-ignored observation of the mechanism
//! both tickets describe (and proves the fixture drives the real binary).
//! `a_task_can_be_closed_without_claiming_done_or_requeueing` is the property
//! the tickets ask for; it is `#[ignore]`d and RED while d8d25af9 is open.
//! (It encodes resolution (a) of d8d25af9 -- a terminal `cancelled` status. If
//! the fix takes resolution (b) instead, rewrite this test against that flag.)
//!
//! Written by an independent closure verifier, not an implementer.

mod common;

use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

struct Fixture {
    home: PathBuf,
    repo: PathBuf,
}

fn unique_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-f6c5164e-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let home = unique_dir(&format!("{tag}-home"));
        let repo = unique_dir(&format!("{tag}-repo"));
        let repo = repo.canonicalize().unwrap();
        // Close-evidence fixture: `add` lands `pending` (the state `fail` acts
        // on) only with a REPRODUCED repro test run from a committed script,
        // so this is a real git repo (was a bare `.git` dir) with one commit.
        std::fs::create_dir_all(repo.join("tests")).unwrap();
        std::fs::write(
            repo.join("tests/repro_yes.sh"),
            "echo 'bug present'; exit 1\n",
        )
        .unwrap();
        for args in [
            &["init", "-q"][..],
            &["add", "tests/repro_yes.sh"],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t.t",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "repro",
            ],
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(&repo)
                .status()
                .unwrap()
                .success());
        }
        Fixture { home, repo }
    }

    fn run(&self, args: &[&str]) -> Out {
        let out = Command::new(env!("CARGO_BIN_EXE_backlog"))
            .env("PATH", common::path_with_condukt_shim())
            .args(args)
            .env("HOME", &self.home)
            .current_dir(&self.repo)
            .stdin(Stdio::null())
            .output()
            .expect("binary runs");
        Out {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn add(&self, title: &str) -> String {
        let project = self.repo.to_string_lossy().into_owned();
        let out = self.run(&[
            "add",
            "--title",
            title,
            "--project",
            &project,
            "--repro-test",
            "bash tests/repro_yes.sh",
        ]);
        assert_eq!(out.code, 0, "add failed: {} {}", out.stdout, out.stderr);
        out.stdout
            .lines()
            .find_map(|l| l.strip_prefix("added: "))
            .unwrap_or_else(|| panic!("no `added: <id>` in {:?}", out.stdout))
            .trim()
            .to_string()
    }

    fn row(&self, id: &str) -> Option<serde_json::Value> {
        let out = self.run(&["list", "--all", "--json"]);
        assert_eq!(out.code, 0, "list failed: {} {}", out.stdout, out.stderr);
        let rows: Vec<serde_json::Value> = serde_json::from_str(out.stdout.trim())
            .unwrap_or_else(|e| panic!("list --json not an array ({e}): {:?}", out.stdout));
        rows.into_iter().find(|r| {
            r["id"]
                .as_str()
                .is_some_and(|s| s.starts_with(id) || id.starts_with(s))
        })
    }
}

#[test]
fn failed_is_always_requeued() {
    let fx = Fixture::new("requeue");
    let id = fx.add("cannot be implemented");
    let out = fx.run(&["fail", &id, "--reason", "decided not to do it"]);
    assert_eq!(out.code, 0, "fail: {} {}", out.stdout, out.stderr);
    let row = fx.row(&id).expect("failed row is listed");
    assert!(
        row["defer_until"].as_i64().is_some(),
        "fail must set defer_until (the re-queue the tickets describe): {row}"
    );
}

#[test]
#[ignore = "d8d25af9 open: no terminal will-not-do state (f6c5164e duplicate)"]
fn a_task_can_be_closed_without_claiming_done_or_requeueing() {
    let fx = Fixture::new("wontfix");
    let id = fx.add("will not do");
    let out = fx.run(&["edit", &id, "--status", "cancelled"]);
    assert_eq!(
        out.code, 0,
        "a permanent will-not-do close must exist; stdout={} stderr={}",
        out.stdout, out.stderr
    );
    if let Some(row) = fx.row(&id) {
        let st = row["status"].as_str().unwrap_or("");
        assert!(
            st != "pending" && st != "failed" && st != "done",
            "closed task must not read as pending/failed/done: {row}"
        );
        assert!(
            row["defer_until"].is_null(),
            "closed task must not re-queue: {row}"
        );
    }
}
