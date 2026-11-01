// 統合テスト: テスト内の unwrap/expect/panic は意図的な assert（production 側は workspace の deny のまま）。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: two live Claude Code processes that share ONE session id (e.g. a
//! `claude --continue` taken over by a second process) are indistinguishable
//! to `overwatch status`. `presence.json` is keyed by `session_id` alone, so
//! the second process's hooks overwrite the first's `cwd` / `worktree` and
//! the paths written by process A are reported as written from process B's
//! worktree.
//!
//! Setup: one repo, one linked worktree. Process A (cwd = the main checkout)
//! writes `a.rs`; process B (cwd = the linked worktree) writes `b.rs`; both
//! use the same session id. Hooks are run through the commands configured in
//! hooks.json (BIN substituted); HOME is a tempdir sandbox. Every child is a
//! short-lived `sh -c` that is waited on to completion (no unbounded wait:
//! the hook reads stdin to EOF, which is closed right after the write).

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_overwatch");
const PLUGIN_BIN: &str = "${CLAUDE_PLUGIN_ROOT}/bin/overwatch";

struct Sandbox {
    _t: TempDir,
    root: PathBuf,
    home: PathBuf,
    repo: PathBuf,
    wt: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn sandbox() -> Sandbox {
    let t = TempDir::new().unwrap();
    let root = t.path().canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("repo");
    let wt = root.join("wt-b");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("README"), "x\n").unwrap();
    git(&repo, &["add", "README"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "side", wt.to_str().unwrap()],
    );
    let wt = wt.canonicalize().unwrap();
    Sandbox {
        _t: t,
        root,
        home,
        repo,
        wt,
    }
}

fn base_cmd(sb: &Sandbox, cwd: &Path) -> Command {
    let mut c = Command::new("sh");
    harness_core::git_env::scrub(&mut c);
    c.current_dir(cwd)
        .env("HOME", &sb.home)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDE_PLUGIN_ROOT", env!("CARGO_MANIFEST_DIR"))
        .env_remove("HARNESS_SANDBOX_HOME")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("OVERWATCH_RUN_ID");
    c
}

/// Commands configured in hooks.json for `event`.
fn hook_commands(event: &str) -> Vec<String> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/hooks.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    let mut out = Vec::new();
    let Some(entries) = v["hooks"][event].as_array() else {
        return out;
    };
    for e in entries {
        for h in e["hooks"].as_array().cloned().unwrap_or_default() {
            if h["type"] == "command" {
                let cmd = h["command"].as_str().unwrap();
                // Only the presence recorder is relevant; skip `status` etc.
                if cmd.contains(" presence") {
                    out.push(cmd.replace(PLUGIN_BIN, BIN));
                }
            }
        }
    }
    out
}

fn fire(sb: &Sandbox, event: &str, payload: &Value) {
    let cwd = PathBuf::from(payload["cwd"].as_str().unwrap());
    let cmds = hook_commands(event);
    assert!(
        !cmds.is_empty(),
        "hooks.json has no presence command for {event}"
    );
    for cmd in cmds {
        let mut child = base_cmd(sb, &cwd)
            .arg("-c")
            .arg(&cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        {
            use std::io::Write;
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(payload.to_string().as_bytes()).unwrap();
        }
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{event} hook command `{cmd}` exited {:?}\nstdout:{}\nstderr:{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn status_json(sb: &Sandbox, cwd: &Path) -> (Value, String) {
    let mut c = Command::new(BIN);
    harness_core::git_env::scrub(&mut c);
    let out = c
        .current_dir(cwd)
        .env("HOME", &sb.home)
        .env("PATH", "/usr/bin:/bin")
        .env_remove("HARNESS_SANDBOX_HOME")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("OVERWATCH_RUN_ID")
        .stdin(Stdio::null())
        .args(["status", "--json"])
        .output()
        .unwrap();
    let raw = String::from_utf8_lossy(&out.stdout).to_string();
    let v: Value = serde_json::from_str(&raw).unwrap_or_else(|e| {
        panic!(
            "status --json stdout is not JSON ({e}):\n{raw}\nstderr:{}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (v, raw)
}

fn write_event(sid: &str, transcript: &Path, cwd: &Path, file: &Path) -> Value {
    json!({"session_id":sid,"transcript_path":transcript,"cwd":cwd,
           "hook_event_name":"PostToolUse","tool_name":"Write",
           "tool_input":{"file_path":file}})
}

/// Proves (if GREEN): when process A (main checkout) and process B (linked
/// worktree) share a session id, `status --json` never reports a path written
/// by A under B's worktree, nor B's path under A's worktree. Control: both
/// paths appear somewhere in `active_sessions` (anti-vacuity: the hooks ran
/// and status reads the registry).
///
/// Does NOT prove: how the fix tells the processes apart (pid, per-process
/// token, ...), SessionEnd handling, or lease (`begin`) behaviour.
#[test]
fn same_session_id_two_processes_paths_attributed_to_own_worktree() {
    let sb = sandbox();
    let sid = "sess-shared-two-procs";
    let tdir = sb.root.join("transcripts");
    std::fs::create_dir_all(&tdir).unwrap();
    let t = tdir.join("shared.jsonl");
    std::fs::write(&t, "{\"type\":\"user\"}\n").unwrap();

    let a_file = sb.repo.join("a.rs");
    let b_file = sb.wt.join("b.rs");
    fire(&sb, "PostToolUse", &write_event(sid, &t, &sb.repo, &a_file));
    fire(&sb, "PostToolUse", &write_event(sid, &t, &sb.wt, &b_file));

    let (v, raw) = status_json(&sb, &sb.repo);
    let entries: Vec<&Value> = v["active_sessions"]
        .as_array()
        .unwrap_or_else(|| panic!("no active_sessions array:\n{raw}"))
        .iter()
        .filter(|e| e["session_id"] == sid)
        .collect();

    let a_s = a_file.to_str().unwrap();
    let b_s = b_file.to_str().unwrap();
    let lists = |e: &Value, p: &str| {
        e["written_paths"]
            .as_array()
            .is_some_and(|a| a.iter().any(|x| x == p))
    };
    let canon = |e: &Value| {
        PathBuf::from(e["worktree"].as_str().unwrap_or(""))
            .canonicalize()
            .unwrap_or_default()
    };

    // Controls: both writes are visible in the registry.
    assert!(
        entries.iter().any(|e| lists(e, a_s)),
        "control: A's write {a_s} missing from active_sessions:\n{raw}"
    );
    assert!(
        entries.iter().any(|e| lists(e, b_s)),
        "control: B's write {b_s} missing from active_sessions:\n{raw}"
    );

    for e in &entries {
        if lists(e, a_s) {
            assert_eq!(
                canon(e),
                sb.repo.canonicalize().unwrap(),
                "path {a_s} was written by the process in {} but is reported under worktree {}:\n{raw}",
                sb.repo.display(),
                e["worktree"]
            );
        }
        if lists(e, b_s) {
            assert_eq!(
                canon(e),
                sb.wt,
                "path {b_s} was written by the process in {} but is reported under worktree {}:\n{raw}",
                sb.wt.display(),
                e["worktree"]
            );
        }
    }
}
