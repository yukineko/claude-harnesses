// 統合テスト: テスト内の unwrap/expect/panic は意図的な assert（production 側は workspace の deny のまま）。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: the overwatch Stop hook runs `overwatch status`, printing the
//! full dashboard at every Stop and pushing the final report out of view.
//!
//! Contract (user ruling 2026-10-27): the Stop hook in
//! `crates/overwatch/hooks/hooks.json` must not run `overwatch status`.
//! `overwatch status` stays available on demand. SessionStart, PostToolUse and
//! SessionEnd keep their `overwatch presence` entries, and SessionStart keeps
//! its `overwatch status` entry.
//!
//! Commands are read from hooks.json with `${CLAUDE_PLUGIN_ROOT}/bin/overwatch`
//! replaced by the built binary; every child runs with `HOME` = a tempdir
//! sandbox and `PATH=/usr/bin:/bin`.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_overwatch");
const PLUGIN_BIN: &str = "${CLAUDE_PLUGIN_ROOT}/bin/overwatch";
const DASHBOARD_HEADER: &str = "== Sessions ==";

struct Sandbox {
    _t: TempDir,
    root: PathBuf,
    home: PathBuf,
    repo: PathBuf,
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
    Sandbox {
        _t: t,
        root,
        home,
        repo,
    }
}

fn hooks_json() -> Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/hooks.json");
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap()
}

/// Raw (unsubstituted) command strings for every entry of `event`, any matcher.
fn raw_commands(event: &str) -> Vec<String> {
    let v = hooks_json();
    let mut out = Vec::new();
    for e in v["hooks"][event].as_array().cloned().unwrap_or_default() {
        for h in e["hooks"].as_array().cloned().unwrap_or_default() {
            if h["type"] == "command" {
                out.push(h["command"].as_str().unwrap().to_string());
            }
        }
    }
    out
}

fn has_subcommand(cmds: &[String], sub: &str) -> bool {
    let needle = format!("overwatch {sub}");
    cmds.iter().any(|c| c.contains(&needle))
}

fn run_shell(sb: &Sandbox, cmd: &str, payload: &Value) -> (Option<i32>, String, String) {
    let mut child = harness_core::git_env::scrub(&mut Command::new("sh"))
        .current_dir(&sb.repo)
        .env("HOME", &sb.home)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDE_PLUGIN_ROOT", env!("CARGO_MANIFEST_DIR"))
        .env_remove("HARNESS_SANDBOX_HOME")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("OVERWATCH_RUN_ID")
        .arg("-c")
        .arg(cmd)
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
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// Proves: no command configured under `hooks.Stop` in hooks.json contains the
/// substring `overwatch status` (structural half of the ruling).
///
/// Does NOT prove: that some other Stop command does not print a dashboard by
/// another route (see the behavioural test below), or anything about other
/// plugins' Stop hooks.
#[test]
fn stop_hook_does_not_run_overwatch_status() {
    let stop = raw_commands("Stop");
    let offending: Vec<&String> = stop
        .iter()
        .filter(|c| c.contains("overwatch status"))
        .collect();
    assert!(
        offending.is_empty(),
        "hooks.json Stop runs `overwatch status` (full dashboard at every Stop): {offending:?}"
    );
}

/// Proves: running every hooks.json Stop command (binary substituted) with a
/// well-formed Stop payload in a sandbox prints, combined, no `== Sessions ==`
/// dashboard header on stdout. The control in the same sandbox proves that
/// `overwatch status` run directly DOES print that header, so the absence is
/// not an artifact of the sandbox making status print nothing.
///
/// Does NOT prove: anything when hooks.json has zero Stop commands (then this
/// half is trivially satisfied and the structural test carries the contract),
/// or real Claude Code Stop payloads (this one is synthetic).
#[test]
fn stop_hook_commands_print_no_dashboard() {
    let sb = sandbox();
    let transcript = sb.root.join("t.jsonl");
    std::fs::write(&transcript, "{\"type\":\"user\"}\n").unwrap();
    let payload = json!({"session_id":"sess-stop-9b2c","transcript_path":transcript,
        "cwd":sb.repo,"hook_event_name":"Stop","stop_hook_active":false});

    // Control: on-demand `overwatch status` still prints the dashboard here.
    let (code, ctl, ctl_err) = run_shell(&sb, &format!("{BIN} status"), &json!({}));
    assert!(
        ctl.contains(DASHBOARD_HEADER),
        "control: `overwatch status` (exit {code:?}) no longer prints {DASHBOARD_HEADER}:\n{ctl}\nstderr:{ctl_err}"
    );

    let mut combined = String::new();
    for cmd in raw_commands("Stop") {
        let cmd = cmd.replace(PLUGIN_BIN, BIN);
        let (_code, out, _err) = run_shell(&sb, &cmd, &payload);
        combined.push_str(&out);
    }
    assert!(
        !combined.contains(DASHBOARD_HEADER),
        "Stop hook commands printed the status dashboard on stdout:\n{combined}"
    );
}

/// Control (must stay GREEN before and after the fix): SessionStart keeps both
/// `overwatch presence` and `overwatch status`; PostToolUse and SessionEnd keep
/// `overwatch presence`. Guards against a fix that strips too much.
///
/// Does NOT prove: matcher contents, timeouts, or ordering of entries.
#[test]
fn other_events_keep_presence_and_session_start_keeps_status() {
    let ss = raw_commands("SessionStart");
    assert!(
        has_subcommand(&ss, "presence"),
        "SessionStart lost `overwatch presence`: {ss:?}"
    );
    assert!(
        has_subcommand(&ss, "status"),
        "SessionStart lost `overwatch status`: {ss:?}"
    );
    for ev in ["PostToolUse", "SessionEnd"] {
        let c = raw_commands(ev);
        assert!(
            has_subcommand(&c, "presence"),
            "{ev} lost `overwatch presence`: {c:?}"
        );
    }
}
