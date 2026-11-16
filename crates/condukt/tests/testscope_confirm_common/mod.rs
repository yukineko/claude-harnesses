//! Shared fixture for the testscope-confirmed-run RED files (spec
//! specs/testscope-confirmed-run.toml, backlog 64fe4e19). Test support only.
//!
//! The payload shapes follow the observed hook contract: PreToolUse /
//! PostToolUse / PostToolUseFailure carry `tool_use_id`, `session_id`,
//! `tool_name`, `tool_input.command` and `cwd`. PreToolUse is always driven
//! through `condukt testscope`. The Post-side entry point does not exist at the
//! base revision and the spec does not name its subcommand, so [`hook_args`]
//! reads it from `hooks/hooks.json` (the `Bash`-matcher entry of that event)
//! and falls back to `testscope` (which at the base revision judges any payload
//! as a Pre, so the tests fail by assertion, not by a missing binary).
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;

pub struct Env {
    pub home: TempDir,
    pub dir: TempDir,
}

impl Env {
    pub fn new() -> Self {
        Env {
            home: tempfile::tempdir().unwrap(),
            dir: tempfile::tempdir().unwrap(),
        }
    }
    pub fn ledger(&self) -> PathBuf {
        self.dir.path().join("ledger.tsv")
    }
    pub fn v2(&self) -> PathBuf {
        self.dir.path().join("ledger.v2.tsv")
    }
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

pub fn repo() -> TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(d.path().join("a.txt"), "one\n").unwrap();
    git(d.path(), &["add", "-A"]);
    git(d.path(), &["commit", "-q", "-m", "init"]);
    d
}

/// Change the tree identity (a tracked edit); `restore` puts it back.
pub fn edit_tree(root: &Path) {
    std::fs::write(root.join("a.txt"), "two\n").unwrap();
}
pub fn restore_tree(root: &Path) {
    std::fs::write(root.join("a.txt"), "one\n").unwrap();
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Ev {
    Pre,
    Post,
    PostFail,
}

impl Ev {
    fn name(self) -> &'static str {
        match self {
            Ev::Pre => "PreToolUse",
            Ev::Post => "PostToolUse",
            Ev::PostFail => "PostToolUseFailure",
        }
    }
}

pub struct Out {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Out {
    pub fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_str(self.stdout.trim()).ok()
    }
    pub fn is_allow(&self) -> bool {
        self.stdout.trim().is_empty()
    }
    /// `permissionDecision` of a hook output, `None` when absent.
    pub fn decision(&self) -> Option<String> {
        self.json()?["hookSpecificOutput"]["permissionDecision"]
            .as_str()
            .map(str::to_string)
    }
    pub fn is_deny(&self) -> bool {
        self.decision().as_deref() == Some("deny")
    }
    pub fn is_ask(&self) -> bool {
        self.decision().as_deref() == Some("ask")
    }
    /// Every human-readable text of the output (reason + systemMessage).
    pub fn texts(&self) -> Vec<String> {
        let Some(v) = self.json() else {
            return vec![];
        };
        let mut t = Vec::new();
        for p in [
            &v["hookSpecificOutput"]["permissionDecisionReason"],
            &v["systemMessage"],
            &v["reason"],
        ] {
            if let Some(s) = p.as_str() {
                t.push(s.to_string());
            }
        }
        t
    }
}

/// The argument list (after `bin/condukt`) of the `Bash`-matcher entry of
/// `event` in hooks/hooks.json, or `["testscope"]` when there is none.
pub fn hook_args(event: &str) -> Vec<String> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/hooks.json");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    if let Some(entries) = v["hooks"][event].as_array() {
        for e in entries {
            let m = e["matcher"].as_str().unwrap_or("");
            if !m.split('|').any(|x| x == "Bash") {
                continue;
            }
            for h in e["hooks"].as_array().into_iter().flatten() {
                if let Some(c) = h["command"].as_str() {
                    if let Some((_, rest)) = c.split_once("bin/condukt") {
                        let a: Vec<String> = rest.split_whitespace().map(str::to_string).collect();
                        if !a.is_empty() {
                            return a;
                        }
                    }
                }
            }
        }
    }
    vec!["testscope".to_string()]
}

pub fn fire_with_path(
    env: &Env,
    ev: Ev,
    sid: &str,
    id: Option<&str>,
    cmd: &str,
    cwd: &Path,
    path: Option<&str>,
) -> Out {
    let mut payload = serde_json::json!({
        "hook_event_name": ev.name(),
        "session_id": sid,
        "tool_name": "Bash",
        "tool_input": {"command": cmd},
        "cwd": cwd.to_str().unwrap(),
    });
    if let Some(id) = id {
        payload["tool_use_id"] = serde_json::json!(id);
    }
    if ev == Ev::PostFail {
        payload["error"] = serde_json::json!("Command failed");
        payload["is_interrupt"] = serde_json::json!(false);
    }
    let args = match ev {
        Ev::Pre => vec!["testscope".to_string()],
        Ev::Post => hook_args("PostToolUse"),
        Ev::PostFail => hook_args("PostToolUseFailure"),
    };
    let mut c = Command::new(env!("CARGO_BIN_EXE_condukt"));
    harness_core::git_env::scrub(&mut c)
        .args(&args)
        .env("HOME", env.home.path())
        .env("CONDUKT_TESTSCOPE_LEDGER", env.ledger())
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env_remove("CONDUKT_DISABLE")
        .env_remove("CONDUKT_WORKTREE_BASE")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(p) = path {
        c.env("PATH", p);
    }
    let mut child = c.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    Out {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

pub fn fire(env: &Env, ev: Ev, sid: &str, id: Option<&str>, cmd: &str, cwd: &Path) -> Out {
    fire_with_path(env, ev, sid, id, cmd, cwd, None)
}

pub fn pre(env: &Env, id: &str, cmd: &str, cwd: &Path) -> Out {
    fire(env, Ev::Pre, "s", Some(id), cmd, cwd)
}
pub fn post(env: &Env, id: &str, cmd: &str, cwd: &Path) -> Out {
    fire(env, Ev::Post, "s", Some(id), cmd, cwd)
}
pub fn post_fail(env: &Env, id: &str, cmd: &str, cwd: &Path) -> Out {
    fire(env, Ev::PostFail, "s", Some(id), cmd, cwd)
}
