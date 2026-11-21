#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Shared black-box helpers: isolated HOME / SESSION_MASTER_HOME, real `git init` repos.

use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tempfile::TempDir;

pub const T0: i64 = 1_800_000_000;

pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Out {
    pub fn json(&self) -> Value {
        serde_json::from_str(self.stdout.trim()).unwrap_or_else(|e| {
            panic!(
                "stdout is not JSON ({e}): {:?} (code {}, stderr {:?})",
                self.stdout, self.code, self.stderr
            )
        })
    }
}

pub struct Sandbox {
    pub tmp: TempDir,
    pub home: PathBuf,
    pub repo: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@e"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(
        o.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&o.stderr)
    );
}

impl Sandbox {
    pub fn new() -> Sandbox {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        let repo = root.join("repo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["commit", "--allow-empty", "-m", "init"]);
        Sandbox { tmp, home, repo }
    }

    /// `git worktree add -b <branch> <tmp>/<dir>`; returns its path.
    pub fn worktree(&self, dir: &str, branch: &str) -> PathBuf {
        let p = self.tmp.path().canonicalize().unwrap().join(dir);
        git(
            &self.repo,
            &["worktree", "add", "-b", branch, p.to_str().unwrap()],
        );
        p
    }

    pub fn run_with(
        &self,
        args: &[&str],
        now: &str,
        extra: &[(&str, &str)],
        stdin: Option<&str>,
    ) -> Out {
        let mut c = Command::new(env!("CARGO_BIN_EXE_session-master"));
        c.args(args)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("SESSION_MASTER_HOME", self.home.join("sm"))
            .env("SESSION_MASTER_NOW", now)
            .env("SESSION_MASTER_TTL_SECS", "60")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in extra {
            c.env(k, v);
        }
        let mut child = c.spawn().expect("spawn session-master");
        if let Some(s) = stdin {
            // The child may exit without reading stdin (stub); ignore EPIPE.
            let _ = child.stdin.take().unwrap().write_all(s.as_bytes());
        }
        let o = child.wait_with_output().unwrap();
        Out {
            code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        }
    }

    /// Run a subcommand as `session` in the main repo dir at time `now`.
    pub fn sm(&self, args: &[&str], session: &str, now: i64) -> Out {
        self.sm_in(args, session, now, &self.repo)
    }

    pub fn sm_in(&self, args: &[&str], session: &str, now: i64, cwd: &Path) -> Out {
        let mut a: Vec<&str> = args.to_vec();
        a.extend(["--session", session, "--cwd", cwd.to_str().unwrap()]);
        self.run_with(&a, &now.to_string(), &[], None)
    }

    pub fn register(&self, session: &str, now: i64) -> Out {
        self.sm(&["register"], session, now)
    }

    pub fn role(&self, session: &str, now: i64) -> Out {
        self.sm(&["role"], session, now)
    }

    pub fn status(&self, session: &str, now: i64) -> Value {
        let o = self.sm(&["status"], session, now);
        assert_eq!(o.code, 0, "status failed: {}", o.stderr);
        o.json()
    }

    pub fn gate_payload(&self, payload: &Value, now: i64) -> Out {
        self.run_with(&["gate"], &now.to_string(), &[], Some(&payload.to_string()))
    }

    pub fn gate_bash(&self, session: &str, cwd: &Path, cmd: &str, now: i64) -> Out {
        self.gate_payload(&bash_payload(session, cwd, cmd), now)
    }

    pub fn gate_send(&self, session: &str, cwd: &Path, to: &str, now: i64) -> Out {
        self.gate_payload(&send_payload(session, cwd, to), now)
    }

    /// Master = A (t0), member = B (t0+1).
    pub fn master_and_member(&self) {
        assert_eq!(self.register("A", T0).code, 0);
        assert_eq!(self.register("B", T0 + 1).code, 0);
    }
}

pub fn bash_payload(session: &str, cwd: &Path, cmd: &str) -> Value {
    json!({
        "session_id": session,
        "cwd": cwd.to_str().unwrap(),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": cmd}
    })
}

pub fn send_payload(session: &str, cwd: &Path, to: &str) -> Value {
    json!({
        "session_id": session,
        "cwd": cwd.to_str().unwrap(),
        "hook_event_name": "PreToolUse",
        "tool_name": "SendMessage",
        "tool_input": {"to": to, "message": "x"}
    })
}

pub fn is_deny(o: &Out) -> bool {
    match serde_json::from_str::<Value>(o.stdout.trim()) {
        Ok(v) => v["hookSpecificOutput"]["permissionDecision"] == "deny",
        Err(_) => false,
    }
}

pub fn deny_reason(o: &Out) -> String {
    o.json()["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap_or("")
        .to_string()
}
