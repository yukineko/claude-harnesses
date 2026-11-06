// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Audit B item 5 (memory `condukt-operator-gotchas`, section "A clean
//! worktree is not proof the worker died"; originally
//! `condukt-redispatch-stuck-worker-dup`), 2026-11-07.
//!
//! `condukt state abandon --all-stuck` is gated on a progress verdict and a
//! clean worktree, but the explicit `--task <id>` arm resets a running task
//! to `pending` with NO liveness check (main.rs `StateAction::Abandon`). The
//! skill then re-dispatches a second worker onto the same branch while the
//! first is still writing: the observed stacked-commit incident.
//!
//! The liveness signal that works (memory `liveness-and-observability`) is
//! the worker's subagent transcript mtime. Claude Code writes it at
//! `$HOME/.claude/projects/<project-slug>/<session-id>/subagents/agent-<agentId>.jsonl`,
//! and the task records `agentId` via `state set --agent-id`. This test puts
//! a FRESH transcript there and requires the explicit reset to refuse. The
//! control puts an OLD transcript there and requires the reset to proceed,
//! so a gate that refuses every explicit reset does not satisfy it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const AGENT: &str = "a0b05f00dfeedbeef";

fn condukt(dir: &Path, home: &Path, args: &[&str]) -> (i32, String) {
    let out = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env_remove("CONDUKT_DISABLE")
        .stdin(Stdio::null())
        .output()
        .expect("condukt");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Claude Code's project slug: every non-alphanumeric byte becomes `-`.
fn slug(p: &Path) -> String {
    p.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Write the worker transcript under both the literal and the canonical
/// project path (macOS temp dirs live behind the `/var` → `/private/var`
/// symlink), so the test does not depend on which spelling a fix resolves.
fn write_transcript(home: &Path, project: &Path, fresh: bool) {
    let mut projects: Vec<PathBuf> = vec![project.to_path_buf()];
    if let Ok(c) = project.canonicalize() {
        projects.push(c);
    }
    for p in projects {
        let dir = home
            .join(".claude/projects")
            .join(slug(&p))
            .join("sess-b05")
            .join("subagents");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join(format!("agent-{AGENT}.jsonl"));
        std::fs::write(&f, "{\"type\":\"assistant\"}\n").unwrap();
        if !fresh {
            let st = Command::new("touch")
                .args(["-t", "202001010000"])
                .arg(&f)
                .status()
                .unwrap();
            assert!(st.success());
        }
    }
}

fn abandon(tag: &str, fresh: bool) -> (i32, String) {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join("run");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let dec = run_dir.join("dec.json");
    std::fs::write(
        &dec,
        serde_json::json!({"goal": "g", "tasks": [{"id": "t1", "title": "work"}]}).to_string(),
    )
    .unwrap();
    let run = format!("run-b05-{tag}");
    let (c, e) = condukt(
        &run_dir,
        &home,
        &[
            "state",
            "init",
            "--run",
            &run,
            "--file",
            dec.to_str().unwrap(),
        ],
    );
    assert_eq!(c, 0, "state init: {e}");
    let (c, e) = condukt(
        &run_dir,
        &home,
        &[
            "state",
            "set",
            "--run",
            &run,
            "--task",
            "t1",
            "--status",
            "running",
            "--agent-id",
            AGENT,
        ],
    );
    assert_eq!(c, 0, "state set running: {e}");
    write_transcript(&home, &run_dir, fresh);
    condukt(
        &run_dir,
        &home,
        &["state", "abandon", "--run", &run, "--task", "t1"],
    )
}

/// Control: the worker's transcript has been idle for years — the explicit
/// human reset must still work.
#[test]
fn explicit_abandon_of_task_with_idle_worker_transcript_proceeds() {
    let (code, stderr) = abandon("idle", false);
    assert_eq!(code, 0, "control must reset; stderr: {stderr}");
}

/// RED: the worker's transcript was written just now — it is alive.
#[test]
fn explicit_abandon_refuses_while_worker_transcript_is_fresh() {
    let (code, stderr) = abandon("fresh", true);
    assert_ne!(
        code, 0,
        "state abandon --task reset a running task whose worker transcript \
         (agent-{AGENT}.jsonl) was modified seconds ago; a second worker would be \
         dispatched onto a live worker's branch. stderr: {stderr}"
    );
}
