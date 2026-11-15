// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (2026-11-07, session c2e3bc17): donegate judges, on the PARENT's
//! Stop, a root that only a still-running background subagent wrote into.
//!
//! Observed: root ~/.condukt/worktrees/run-20261107-034825-70254-c2-debc6df3
//! was mid-edit by a running subagent (a test-writer had swapped in the pre-fix
//! git.rs to watch pins fail, so the tree was not the fix candidate). donegate
//! ran lint-changed/test-changed on it at the parent's Stop and blocked the
//! parent three times (attempts 1-3) and gave up at attempt 4, reporting
//! mid-edit state to an agent that did not own the root and could not fix it.
//! User ruling 2026-11-07: 「lintと推論が競合しているのは解消したい」.
//!
//! Mechanism: harness_core::session_roots::resolve takes every path written in
//! the parent transcript AND in every subagent sidechain
//! (`<transcript-stem>/subagents/*.jsonl`, via
//! attribution::session_written_paths) as a root of the parent's Stop, with no
//! notion of who owns the root or whether that owner is still running. And
//! donegate registers only a `Stop` hook (crates/donegate/hooks/hooks.json),
//! so the owner's own stop (SubagentStop) never checks it.
//!
//! The in-flight signal used here is the one Claude Code actually records
//! (shape copied from the observed parent transcript): the Agent tool_use's
//! tool_result carries `toolUseResult {isAsync:true, status:"async_launched",
//! agentId}`, the sidechain's `agent-<id>.meta.json` carries the `toolUseId`,
//! and no `<task-notification>` for that agent has arrived yet.
//!
//! Desired (2026-11-07): such a root is not judged at the parent's Stop (it is
//! judged at the owner's SubagentStop); not silently — the root is named in
//! the output. Fail-closed controls: when liveness cannot be determined, or
//! the parent itself also wrote into the root, it still blocks.
//!
//! SUPERSEDED by user rulings 2026-11-08, verbatim: "donegateのworktreeテストは
//! 問題ありすぎるので即時に停止" and "donegateそのものが他のworktreeを見たり検証
//! したりテストしたりすることは禁止とする". donegate judges only the hook cwd; it
//! never runs, blocks on or names another worktree, so the deferral (and its
//! liveness / ownership controls) no longer exists. The three parent-Stop
//! tests are re-scoped to that contract; the SubagentStop registration test is
//! unchanged.
//!
//! Written by a disinterested author (CLAUDE.md section 2(a)): no fix exists.
//! Re-scoped 2026-11-08 by the implementer of the ruling (not disinterested).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

const AGENT_ID: &str = "a0123456789abcdef";
const TOOL_USE_ID: &str = "toolu_inflight_0001";

struct Fixture {
    home: PathBuf,
    primary: PathBuf,
    wt: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let st = harness_core::git_env::scrub(&mut Command::new("git"))
        .current_dir(dir)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git runs");
    assert!(st.success(), "git {args:?} failed in {}", dir.display());
}

fn line(v: serde_json::Value) -> String {
    let mut s = v.to_string();
    s.push('\n');
    s
}

fn edit_use(id: &str, path: &Path, sidechain: bool) -> String {
    line(serde_json::json!({
        "parentUuid": "p", "isSidechain": sidechain,
        "message": {"model": "claude-opus-5", "id": format!("m-{id}"), "type": "message",
            "role": "assistant",
            "content": [{"type": "tool_use", "id": id, "name": "Edit",
                "input": {"file_path": path.to_string_lossy(),
                          "old_string": "a", "new_string": "b"}}]}
    }))
}

struct Outcome {
    decision: String,
    stdout: String,
    stderr: String,
}

impl Fixture {
    fn new() -> Fixture {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!("dg-inflight-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let primary = home.join("primary");
        std::fs::create_dir_all(&primary).unwrap();
        git(&primary, &["init", "-q"]);
        git(&primary, &["config", "user.email", "t@t.com"]);
        git(&primary, &["config", "user.name", "t"]);
        std::fs::write(primary.join("seed.rs"), "fn seed() {}\n").unwrap();
        std::fs::write(
            primary.join("donegate.toml"),
            "max_attempts = 50\n\n[[check]]\nname = \"primary-ok\"\ncmd = \"exit 0\"\n",
        )
        .unwrap();
        git(&primary, &["add", "seed.rs", "donegate.toml"]);
        git(
            &primary,
            &["-c", "commit.gpgsign=false", "commit", "-qm", "init"],
        );
        let wt = home.join("subagent-wt");
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "wt-branch",
                wt.to_str().unwrap(),
            ],
        );
        // The subagent's mid-edit tree: its check is red right now.
        std::fs::write(
            wt.join("donegate.toml"),
            "max_attempts = 50\n\n[[check]]\nname = \"wt-check\"\n\
             cmd = \"echo ran > wt-check-ran.txt; echo 'mid-edit tree is red'; exit 1\"\n",
        )
        .unwrap();
        std::fs::write(wt.join("mid_edit.rs"), "fn half_done() {}\n").unwrap();
        let primary = primary.canonicalize().unwrap();
        let wt = wt.canonicalize().unwrap();
        std::fs::create_dir_all(home.join(".harness")).unwrap();
        std::fs::write(
            home.join(".harness/trust.toml"),
            format!(
                "trusted = [\"{}\", \"{}\"]\n",
                primary.display(),
                wt.display()
            ),
        )
        .unwrap();
        Fixture { home, primary, wt }
    }

    fn project_dir(&self) -> PathBuf {
        let d = self.home.join(".claude/projects/-slug");
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Parent transcript: spawns a background Agent (async_launched) and,
    /// optionally, itself edits a file in the worktree. Returns its path.
    fn parent_transcript(&self, launch_record: bool, parent_edits_wt: bool) -> PathBuf {
        let p = self.project_dir().join("sess.jsonl");
        let mut body = line(serde_json::json!({
            "type": "user", "message": {"role": "user", "content": "go"}
        }));
        if launch_record {
            body.push_str(&line(serde_json::json!({
                "parentUuid": "p", "isSidechain": false,
                "message": {"model": "claude-opus-5", "id": "m-agent", "type": "message",
                    "role": "assistant",
                    "content": [{"type": "tool_use", "id": TOOL_USE_ID, "name": "Agent",
                        "input": {"description": "edit the worktree",
                                  "subagent_type": "general-purpose",
                                  "prompt": "work in the worktree"}}]}
            })));
            body.push_str(&line(serde_json::json!({
                "parentUuid": "p", "isSidechain": false, "type": "user",
                "message": {"role": "user", "content": [{
                    "tool_use_id": TOOL_USE_ID, "type": "tool_result",
                    "content": [{"type": "text", "text": format!(
                        "Async agent launched successfully.\nagentId: {AGENT_ID}\n\
                         The agent is working in the background.")}]}]},
                "toolUseResult": {"isAsync": true, "status": "async_launched",
                                  "agentId": AGENT_ID}
            })));
        }
        if parent_edits_wt {
            body.push_str(&edit_use("t-parent", &self.wt.join("mid_edit.rs"), false));
        }
        std::fs::write(&p, body).unwrap();
        p
    }

    /// The running subagent's sidechain: it (and only it) edited the worktree.
    fn sidechain(&self, with_meta: bool) {
        let dir = self.project_dir().join("sess").join("subagents");
        std::fs::create_dir_all(&dir).unwrap();
        let mut body = line(serde_json::json!({
            "isSidechain": true, "type": "user",
            "message": {"role": "user", "content": "work in the worktree"}
        }));
        body.push_str(&edit_use("t-sub", &self.wt.join("mid_edit.rs"), true));
        std::fs::write(dir.join(format!("agent-{AGENT_ID}.jsonl")), body).unwrap();
        if with_meta {
            std::fs::write(
                dir.join(format!("agent-{AGENT_ID}.meta.json")),
                serde_json::json!({
                    "agentType": "general-purpose", "description": "edit the worktree",
                    "toolUseId": TOOL_USE_ID, "spawnDepth": 1,
                    "requestShape": "background", "requestNonInteractive": true
                })
                .to_string(),
            )
            .unwrap();
        }
    }

    fn stop(&self, transcript: &Path) -> Outcome {
        let cwd = &self.primary;
        let payload = serde_json::json!({
            "session_id": "s-inflight",
            "transcript_path": transcript.to_string_lossy(),
            "cwd": cwd.to_string_lossy(),
            "hook_event_name": "Stop",
            "stop_hook_active": false,
        })
        .to_string();
        let mut child =
            harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_donegate")))
                .arg("gate")
                .current_dir(cwd)
                .env("HOME", &self.home)
                .env("CLAUDE_CODE_SESSION_ID", "s-inflight")
                .env_remove("DONEGATE_DISABLE")
                .env_remove("HARNESS_TRUST_ALL")
                .env_remove("HARNESS_WORKTREE_ORIGIN_DIR")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("donegate spawns");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(0), "Stop hook must exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let decision = serde_json::from_str::<serde_json::Value>(stdout.trim())
            .ok()
            .and_then(|v| v.get("decision").and_then(|d| d.as_str()).map(String::from))
            .unwrap_or_default();
        Outcome {
            decision,
            stdout,
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// The contract every parent-Stop test below now pins, under user rulings
/// 2026-11-08 ("donegateのworktreeテストは問題ありすぎるので即時に停止";
/// "donegateそのものが他のworktreeを見たり検証したりテストしたりすることは禁止とする"):
/// the parent's Stop at cwd = primary never runs the worktree's red check,
/// never blocks on it, and never names it — whoever wrote it and whether or
/// not its writer is still running.
fn assert_worktree_not_judged(f: &Fixture, o: &Outcome) {
    assert_ne!(
        o.decision, "block",
        "the parent's Stop blocked on another worktree. stdout={:?} stderr={:?}",
        o.stdout, o.stderr
    );
    assert!(
        !f.wt.join("wt-check-ran.txt").exists(),
        "the other worktree's check ran. stdout={:?} stderr={:?}",
        o.stdout,
        o.stderr
    );
    let wt = f.wt.display().to_string();
    assert!(
        !o.stdout.contains(&wt) && !o.stderr.contains(&wt),
        "donegate must not report on another worktree ({wt}). stdout={:?} stderr={:?}",
        o.stdout,
        o.stderr
    );
}

/// Re-scoped by user rulings 2026-11-08 (see [`assert_worktree_not_judged`]).
/// Was: the in-flight subagent's root is deferred AND named in the output.
/// Now: it is not judged and not named.
#[test]
fn parent_stop_does_not_judge_a_root_only_an_inflight_subagent_wrote() {
    let f = Fixture::new();
    let tp = f.parent_transcript(true, false);
    f.sidechain(true);
    let o = f.stop(&tp);
    assert_worktree_not_judged(&f, &o);
}

/// The other half: the root must be judged at its owner's stop, which needs a
/// SubagentStop hook. donegate registers only Stop today.
#[test]
fn donegate_registers_a_subagent_stop_hook() {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/hooks.json");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    let hooks = v.get("hooks").expect("hooks.json has a hooks object");
    assert!(
        hooks
            .get("SubagentStop")
            .is_some_and(|h| h.to_string().contains("donegate")),
        "{} registers no SubagentStop hook running donegate; registered events: {:?}",
        p.display(),
        hooks
            .as_object()
            .map(|o| o.keys().cloned().collect::<Vec<_>>())
    );
}

/// Re-scoped by user rulings 2026-11-08 (see [`assert_worktree_not_judged`]).
/// Was `control_undetermined_liveness_still_blocks`: liveness unknown kept the
/// other worktree judged and blocking. Liveness is no longer consulted at all.
#[test]
fn undetermined_liveness_does_not_judge_another_worktree() {
    let f = Fixture::new();
    let tp = f.parent_transcript(false, false);
    f.sidechain(false);
    let o = f.stop(&tp);
    assert_worktree_not_judged(&f, &o);
}

/// Re-scoped by user rulings 2026-11-08 (see [`assert_worktree_not_judged`]).
/// Was `control_parent_also_wrote_the_root_still_blocks`: a worktree the
/// parent wrote itself was judged and blocked. Now the parent writing it does
/// not make donegate look at it either.
#[test]
fn parent_also_wrote_another_worktree_still_not_judged() {
    let f = Fixture::new();
    let tp = f.parent_transcript(true, true);
    f.sidechain(true);
    let o = f.stop(&tp);
    assert_worktree_not_judged(&f, &o);
}
