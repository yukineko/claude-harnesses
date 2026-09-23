//! Supplementary RED/GREEN coverage for backlog 45c3a699 (condukt task
//! `backlog-done-split`), independently verifying two terminal-monotonicity
//! paths the original RED suite (`tests/done_split.rs`, commit a8a7c3f3) did
//! not exercise: `backlog fail <id>` and `backlog edit --status failed <id>`
//! against an already-done task. Both must be REFUSED (non-zero exit, task
//! still `done`) -- `store::mark_failed` and `store::edit` each check
//! `is_terminal_status` before mutating (see `crates/backlog/src/store.rs`).
//!
//! This file drives the real built binary, mirroring the fixture shape of
//! `tests/done_split.rs` so it is self-contained (integration tests each
//! compile as a separate binary; there is no shared `mod` between them).
//!
//! Written by an independent verifier, not the implementer (CLAUDE.md 2a).
//! Confirmed RED (both new assertions fail) against a8a7c3f3 (RED-suite-only
//! commit, terminal-refusal branches absent) and GREEN against cf5237d0
//! (implementation commit) in a scratch detached worktree.

use std::io::Write;
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
        "backlog-donesplit-verifier-{}-{}-{}",
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
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        // Canonicalize so the project label matches what the binary resolves
        // (macOS temp dirs sit behind the /var -> /private/var symlink).
        let repo = repo.canonicalize().unwrap();
        Fixture { home, repo }
    }

    fn project(&self) -> String {
        self.repo.to_string_lossy().into_owned()
    }

    fn run(&self, args: &[&str]) -> Out {
        let bin = env!("CARGO_BIN_EXE_backlog");
        let mut child = Command::new(bin)
            .args(args)
            .env("HOME", &self.home)
            .current_dir(&self.repo)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("binary spawns");
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(b"");
        }
        let out = child.wait_with_output().expect("binary runs");
        Out {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn add(&self, title: &str) -> String {
        let project = self.project();
        let out = self.run(&["add", "--title", title, "--project", &project]);
        assert_eq!(
            out.code, 0,
            "add {title:?} must succeed; stdout={} stderr={}",
            out.stdout, out.stderr
        );
        out.stdout
            .lines()
            .find_map(|l| l.strip_prefix("added: "))
            .unwrap_or_else(|| panic!("no `added: <id>` line in {:?}", out.stdout))
            .trim()
            .to_string()
    }

    fn done(&self, id: &str) {
        let out = self.run(&["done", id]);
        assert_eq!(
            out.code, 0,
            "done {id} must succeed; stdout={} stderr={}",
            out.stdout, out.stderr
        );
    }

    fn list_json(&self, status: Option<&str>) -> Vec<serde_json::Value> {
        let mut args = vec!["list", "--all", "--json"];
        if let Some(s) = status {
            args.push("--status");
            args.push(s);
        }
        let out = self.run(&args);
        assert_eq!(
            out.code, 0,
            "list {args:?} must succeed; stdout={} stderr={}",
            out.stdout, out.stderr
        );
        serde_json::from_str::<Vec<serde_json::Value>>(out.stdout.trim())
            .unwrap_or_else(|e| panic!("list --json is not a JSON array ({e}): {:?}", out.stdout))
    }
}

#[test]
fn fail_cannot_reopen_a_done_task() {
    let fx = Fixture::new("failnoreopen");
    let a = fx.add("Must stay done under fail");
    fx.done(&a);

    let out = fx.run(&["fail", &a, "--reason", "trying to reopen"]);
    assert_ne!(
        out.code, 0,
        "`fail` on a done task must be REFUSED; stdout={} stderr={}",
        out.stdout, out.stderr
    );

    let done = fx.list_json(Some("done"));
    assert!(
        done.iter().any(|t| t["id"] == a.as_str()),
        "the task must still be done after the refused `fail`; got {done:?}"
    );
    let failed = fx.list_json(Some("failed"));
    assert!(
        !failed.iter().any(|t| t["id"] == a.as_str()),
        "the task must NOT appear as failed; got {failed:?}"
    );
}

#[test]
fn edit_status_failed_cannot_reopen_a_done_task() {
    let fx = Fixture::new("editfailnoreopen");
    let a = fx.add("Must stay done under edit --status failed");
    fx.done(&a);

    let out = fx.run(&["edit", &a, "--status", "failed"]);
    assert_ne!(
        out.code, 0,
        "`edit --status failed` on a done task must be REFUSED; stdout={} stderr={}",
        out.stdout, out.stderr
    );

    let done = fx.list_json(Some("done"));
    assert!(
        done.iter().any(|t| t["id"] == a.as_str()),
        "the task must still be done after the refused edit; got {done:?}"
    );
    let failed = fx.list_json(Some("failed"));
    assert!(
        !failed.iter().any(|t| t["id"] == a.as_str()),
        "the task must NOT appear as failed; got {failed:?}"
    );
}
