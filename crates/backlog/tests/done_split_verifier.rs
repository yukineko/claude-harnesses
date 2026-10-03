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

mod common;

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
    doc_commit: String,
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

/// Close-evidence (2026-10-01): `add` lands `pending` only with a REPRODUCED
/// repro test, and `done` needs recorded evidence. These fixtures are
/// therefore REAL git repos (not a bare `.git` dir) holding a committed repro
/// script and a committed doc-only commit; `add` passes the repro and `done`
/// closes with `--doc-only`. What these tests pin (the done-file split) is
/// unchanged.
fn fixture_git(repo: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A committed repro script (exit 1 = reproduced) + one doc-only commit, in
/// `repo`, which is already a LINKED worktree (`common::linked_checkout`;
/// store writes are refused in a primary tree, 1e6f00ae). Returns the
/// doc-only commit id.
fn init_evidence_repo(repo: &std::path::Path) -> String {
    std::fs::create_dir_all(repo.join("tests")).unwrap();
    std::fs::write(
        repo.join("tests/repro.sh"),
        "#!/bin/bash\necho 'bug present'\nexit 1\n",
    )
    .unwrap();
    fixture_git(repo, &["add", "--", "tests/repro.sh"]);
    fixture_git(repo, &["commit", "-q", "--no-verify", "-m", "repro"]);
    std::fs::create_dir_all(repo.join("docs")).unwrap();
    std::fs::write(repo.join("docs/closed.md"), "# closed\n").unwrap();
    fixture_git(repo, &["add", "--", "docs/closed.md"]);
    fixture_git(repo, &["commit", "-q", "--no-verify", "-m", "doc"]);
    fixture_git(repo, &["rev-parse", "HEAD"])
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let home = unique_dir(&format!("{tag}-home"));
        let repo = unique_dir(&format!("{tag}-repo"));
        common::linked_checkout(&repo);
        // Canonicalize so the project label matches what the binary resolves
        // (macOS temp dirs sit behind the /var -> /private/var symlink).
        let repo = repo.canonicalize().unwrap();
        let doc_commit = init_evidence_repo(&repo);
        Fixture {
            home,
            repo,
            doc_commit,
        }
    }

    fn project(&self) -> String {
        self.repo.to_string_lossy().into_owned()
    }

    fn run(&self, args: &[&str]) -> Out {
        let bin = env!("CARGO_BIN_EXE_backlog");
        let mut child = Command::new(bin)
            .env("PATH", common::path_with_condukt_shim())
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
        let out = self.run(&[
            "add",
            "--title",
            title,
            "--project",
            &project,
            "--repro-test",
            "bash tests/repro.sh",
        ]);
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
        let out = self.run(&["done", id, "--doc-only", &self.doc_commit]);
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
