// このファイルは丸ごと integration test なので unwrap/expect を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! backlog 8a4fe104: `claim::backlog_pending` maps a failed / unparseable
//! `backlog list --status pending --json` to an empty Vec, so
//! `condukt state execution-state` renders "could not read the queue" exactly
//! like "no pending tasks" and exits 0.
//!
//! Required property: when the queue cannot be read, `state execution-state`
//! must NOT succeed — the two answers must be distinguishable by EXIT STATUS
//! (not by stderr wording). The controls prove the `backlog` PATH shim is the
//! binary actually consulted (a shim-emitted row is joined into the output) and
//! that a healthy empty queue still succeeds.
//!
//! The `backlog` binary is resolved plugin-cache-first, then `$PATH`. HOME is a
//! fresh temp dir, so the cache is empty and the PATH shim is what runs.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

struct Fx {
    base: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl Fx {
    /// Temp HOME + git repo, with one task claimed under title `t`.
    fn new(tag: &str) -> Self {
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-8a4fe104-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(home.join(".condukt").join("state")).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t.t"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        let fx = Self { base, repo, home };
        let claim = fx.run(
            &[
                "state",
                "claim-task",
                "--run",
                "r1",
                "--session",
                "s1",
                "--hashkey",
                "abc123",
                "--title",
                "t",
            ],
            None,
        );
        assert!(
            claim.status.success(),
            "precondition: claim-task: {claim:?}"
        );
        fx
    }

    /// Install a `backlog` shim whose body is `script` and return its dir.
    fn shim(&self, script: &str) -> PathBuf {
        let dir = self.home.join("shim");
        std::fs::create_dir_all(&dir).unwrap();
        let sh = dir.join("backlog");
        std::fs::write(&sh, format!("#!/bin/sh\n{script}\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sh, std::fs::Permissions::from_mode(0o755)).unwrap();
        dir
    }

    fn run(&self, args: &[&str], path_prefix: Option<&Path>) -> Output {
        let mut c = Command::new(bin());
        c.args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("HARNESS_PROGRESS_WINDOW_SECS", "0");
        if let Some(p) = path_prefix {
            let old = std::env::var("PATH").unwrap_or_default();
            c.env("PATH", format!("{}:{}", p.display(), old));
        }
        c.output().expect("spawn condukt")
    }

    fn execution_state_with(&self, script: &str) -> Output {
        let dir = self.shim(script);
        self.run(&["state", "execution-state"], Some(&dir))
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn show(o: &Output) -> String {
    format!(
        "status={:?}\nstdout:\n{}\nstderr:\n{}",
        o.status,
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// Control A: the shim is the binary consulted — a pending row it emits with
/// the claimed title is joined into the output (backlog_id appears).
#[test]
fn control_shim_row_is_joined_into_execution_state() {
    let fx = Fx::new("joined");
    let out = fx.execution_state_with(
        r#"echo '[{"id":"bk-shim-8a4f","title":"t","project":"p","status":"pending"}]'"#,
    );
    assert!(
        out.status.success(),
        "healthy backlog: execution-state must succeed: {}",
        show(&out)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("bk-shim-8a4f"),
        "the PATH shim was not consulted (its row was not joined): {}",
        show(&out)
    );
}

/// Control B: a healthy, genuinely empty queue (`[]`, exit 0) succeeds.
#[test]
fn control_empty_queue_succeeds() {
    let fx = Fx::new("empty");
    let out = fx.execution_state_with("echo '[]'");
    assert!(
        out.status.success(),
        "an empty but READABLE queue must succeed: {}",
        show(&out)
    );
}

/// (1) `backlog list` exits non-zero: the queue could not be read.
#[test]
fn backlog_list_nonzero_exit_fails_execution_state() {
    let fx = Fx::new("exit3");
    let out = fx.execution_state_with("echo 'queue unreadable' >&2\nexit 3");
    assert!(
        !out.status.success(),
        "`backlog list` exited 3 but execution-state exited 0 — an unreadable \
         queue is indistinguishable from 'no pending tasks': {}",
        show(&out)
    );
}

/// (2) `backlog list` exits 0 but prints non-JSON: the queue could not be read.
#[test]
fn backlog_list_garbage_output_fails_execution_state() {
    let fx = Fx::new("garbage");
    let out = fx.execution_state_with("echo 'this is not json {'");
    assert!(
        !out.status.success(),
        "`backlog list` printed unparseable output but execution-state exited 0: {}",
        show(&out)
    );
}

/// (3) `backlog list` prints a JSON object with no `tasks` array: the shape is
/// unrecognised, so the pending set is undetermined, not empty.
#[test]
fn backlog_list_object_without_tasks_fails_execution_state() {
    let fx = Fx::new("notasks");
    let out = fx.execution_state_with(r#"echo '{"error":"locked"}'"#);
    assert!(
        !out.status.success(),
        "`backlog list` printed an object lacking `tasks` but execution-state exited 0: {}",
        show(&out)
    );
}

/// (a) `backlog list` prints a JSON scalar: not a queue shape, undetermined.
#[test]
fn backlog_list_json_scalar_fails_execution_state() {
    let fx = Fx::new("scalar");
    let out = fx.execution_state_with("echo '42'");
    assert!(
        !out.status.success(),
        "`backlog list` printed a JSON scalar but execution-state exited 0: {}",
        show(&out)
    );
}

/// (b) `backlog list` prints an object whose `tasks` is not an array.
#[test]
fn backlog_list_tasks_not_array_fails_execution_state() {
    let fx = Fx::new("tasksstr");
    let out = fx.execution_state_with(r#"echo '{"tasks": "nope"}'"#);
    assert!(
        !out.status.success(),
        "`backlog list` printed `tasks` as a non-array but execution-state exited 0: {}",
        show(&out)
    );
}

/// (c) `backlog list` prints an array whose rows are not objects.
#[test]
fn backlog_list_non_object_rows_fail_execution_state() {
    let fx = Fx::new("nonobj");
    let out = fx.execution_state_with("echo '[1, 2]'");
    assert!(
        !out.status.success(),
        "`backlog list` printed non-object rows but execution-state exited 0: {}",
        show(&out)
    );
}

/// (d) `backlog list` prints an object row with no string `title`: the join key
/// is missing (schema drift), so the queue cannot be joined — undetermined.
#[test]
fn backlog_list_row_without_title_fails_execution_state() {
    let fx = Fx::new("notitle");
    let out = fx.execution_state_with(r#"echo '[{"id":"x","status":"pending"}]'"#);
    assert!(
        !out.status.success(),
        "`backlog list` printed a row lacking a string `title` but execution-state exited 0: {}",
        show(&out)
    );
}
