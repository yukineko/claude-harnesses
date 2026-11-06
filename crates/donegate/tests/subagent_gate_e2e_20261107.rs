// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! End-to-end pins for `donegate subagent-gate` (the SubagentStop hook,
//! backlog 95deaa32), run against the real binary (CARGO_BIN_EXE_donegate).
//!
//! Only behavior is asserted (decision JSON, exit code, presence of a
//! systemMessage), never message wording.
//!
//! Written by a disinterested author (CLAUDE.md section 2(a)).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

const AGENT_ID: &str = "a0123456789abcdef";
const SESSION: &str = "s-subgate";

struct Fixture {
    home: PathBuf,
    root: PathBuf,
}

struct Outcome {
    code: Option<i32>,
    decision: String,
    system_message: String,
    stdout: String,
    stderr: String,
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

fn user_line() -> String {
    line(serde_json::json!({
        "type": "user", "message": {"role": "user", "content": "go"}
    }))
}

impl Fixture {
    /// A trusted git root whose single check is red until `home/green.flag`
    /// exists (the flag lives outside the tree so toggling it changes no
    /// tracked/untracked file of the root).
    fn new() -> Fixture {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!("dg-subgate-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let root = home.join("root");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.email", "t@t.com"]);
        git(&root, &["config", "user.name", "t"]);
        std::fs::write(root.join("work.rs"), "fn work() {}\n").unwrap();
        let root = root.canonicalize().unwrap();
        let home = home.canonicalize().unwrap();
        let flag = home.join("green.flag");
        std::fs::write(
            root.join("donegate.toml"),
            format!(
                "max_attempts = 50\n\n[[check]]\nname = \"flag-check\"\n\
                 cmd = \"echo 'flag check is red'; test -f {}\"\n",
                flag.display()
            ),
        )
        .unwrap();
        git(&root, &["add", "work.rs", "donegate.toml"]);
        git(
            &root,
            &["-c", "commit.gpgsign=false", "commit", "-qm", "init"],
        );
        std::fs::create_dir_all(home.join(".harness")).unwrap();
        std::fs::write(
            home.join(".harness/trust.toml"),
            format!("trusted = [\"{}\"]\n", root.display()),
        )
        .unwrap();
        Fixture { home, root }
    }

    fn project_dir(&self) -> PathBuf {
        let d = self.home.join(".claude/projects/-slug");
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn parent_transcript(&self, parent_edits_root: bool) -> PathBuf {
        let p = self.project_dir().join("sess.jsonl");
        let mut body = user_line();
        if parent_edits_root {
            body.push_str(&edit_use("t-parent", &self.root.join("work.rs"), false));
        }
        std::fs::write(&p, body).unwrap();
        p
    }

    /// The subagent's sidechain at the derived location
    /// `<stem>/subagents/agent-<id>.jsonl`; edits the root iff `writes`.
    fn sidechain(&self, writes: bool) -> PathBuf {
        let dir = self.project_dir().join("sess").join("subagents");
        std::fs::create_dir_all(&dir).unwrap();
        let mut body = user_line();
        if writes {
            body.push_str(&edit_use("t-sub", &self.root.join("work.rs"), true));
        } else {
            // A read-only tool call: nothing written.
            body.push_str(&line(serde_json::json!({
                "parentUuid": "p", "isSidechain": true,
                "message": {"model": "claude-opus-5", "id": "m-read", "type": "message",
                    "role": "assistant",
                    "content": [{"type": "tool_use", "id": "t-read", "name": "Read",
                        "input": {"file_path": self.root.join("work.rs").to_string_lossy()}}]}
            })));
        }
        let p = dir.join(format!("agent-{AGENT_ID}.jsonl"));
        std::fs::write(&p, body).unwrap();
        p
    }

    fn set_green(&self, green: bool) {
        let flag = self.home.join("green.flag");
        if green {
            std::fs::write(flag, "").unwrap();
        } else {
            let _ = std::fs::remove_file(flag);
        }
    }

    fn run(&self, sub: &str, payload: serde_json::Value) -> Outcome {
        let mut child =
            harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_donegate")))
                .arg(sub)
                .current_dir(&self.root)
                .env("HOME", &self.home)
                .env("CLAUDE_CODE_SESSION_ID", SESSION)
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
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let v = serde_json::from_str::<serde_json::Value>(stdout.trim()).ok();
        let get = |k: &str| {
            v.as_ref()
                .and_then(|v| v.get(k))
                .and_then(|d| d.as_str())
                .map(String::from)
                .unwrap_or_default()
        };
        Outcome {
            code: out.status.code(),
            decision: get("decision"),
            system_message: get("systemMessage"),
            stdout,
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn subagent_stop(&self, parent: &Path, agent: &Path) -> Outcome {
        self.run(
            "subagent-gate",
            serde_json::json!({
                "session_id": SESSION,
                "transcript_path": parent.to_string_lossy(),
                "agent_transcript_path": agent.to_string_lossy(),
                "agent_id": AGENT_ID,
                "cwd": self.root.to_string_lossy(),
                "hook_event_name": "SubagentStop",
                "stop_hook_active": false,
            }),
        )
    }

    fn parent_stop(&self, parent: &Path) -> Outcome {
        self.run(
            "gate",
            serde_json::json!({
                "session_id": SESSION,
                "transcript_path": parent.to_string_lossy(),
                "cwd": self.root.to_string_lossy(),
                "hook_event_name": "Stop",
                "stop_hook_active": false,
            }),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn dump(o: &Outcome) -> String {
    format!("stdout={:?} stderr={:?}", o.stdout, o.stderr)
}

/// (1) A subagent that wrote into a root whose check is red: its stop blocks.
#[test]
fn red_root_written_by_subagent_blocks_its_stop() {
    let f = Fixture::new();
    let parent = f.parent_transcript(false);
    let agent = f.sidechain(true);
    let o = f.subagent_stop(&parent, &agent);
    assert_eq!(o.code, Some(0), "{}", dump(&o));
    assert_eq!(o.decision, "block", "{}", dump(&o));
}

/// (2) The identical stop repeated (same agent, same tree, same output) is
/// waived: not a block, and loudly (non-empty systemMessage).
#[test]
fn identical_repeat_stop_is_waived_with_a_system_message() {
    let f = Fixture::new();
    let parent = f.parent_transcript(false);
    let agent = f.sidechain(true);
    let first = f.subagent_stop(&parent, &agent);
    assert_eq!(first.decision, "block", "{}", dump(&first));
    let second = f.subagent_stop(&parent, &agent);
    assert_eq!(second.code, Some(0), "{}", dump(&second));
    assert_ne!(second.decision, "block", "{}", dump(&second));
    assert!(
        !second.system_message.trim().is_empty(),
        "a waived repeat must be visible via systemMessage. {}",
        dump(&second)
    );
}

/// (3) Once the check is green the stop is allowed.
#[test]
fn green_after_red_allows_the_stop() {
    let f = Fixture::new();
    let parent = f.parent_transcript(false);
    let agent = f.sidechain(true);
    let first = f.subagent_stop(&parent, &agent);
    assert_eq!(first.decision, "block", "{}", dump(&first));
    f.set_green(true);
    let o = f.subagent_stop(&parent, &agent);
    assert_eq!(o.code, Some(0), "{}", dump(&o));
    assert_ne!(o.decision, "block", "{}", dump(&o));
}

/// (4a) An agent_transcript_path naming a file that does not exist: what the
/// agent wrote cannot be determined, so the stop is blocked (fail-closed).
#[test]
fn missing_agent_transcript_blocks() {
    let f = Fixture::new();
    let parent = f.parent_transcript(false);
    let missing = f.project_dir().join("no/such/agent-gone.jsonl");
    let o = f.subagent_stop(&parent, &missing);
    assert_eq!(o.code, Some(0), "{}", dump(&o));
    assert_eq!(o.decision, "block", "{}", dump(&o));
}

/// (4b) Unlocatable: no agent_transcript_path and no agent_id (so the
/// `<stem>/subagents/agent-<id>.jsonl` path cannot be derived) -> block.
#[test]
fn unlocatable_agent_transcript_blocks() {
    let f = Fixture::new();
    let parent = f.parent_transcript(false);
    let _ = f.sidechain(true);
    let o = f.run(
        "subagent-gate",
        serde_json::json!({
            "session_id": SESSION,
            "transcript_path": parent.to_string_lossy(),
            "cwd": f.root.to_string_lossy(),
            "hook_event_name": "SubagentStop",
            "stop_hook_active": false,
        }),
    );
    assert_eq!(o.code, Some(0), "{}", dump(&o));
    assert_eq!(o.decision, "block", "{}", dump(&o));
}

/// (4c) Derivation works: no agent_transcript_path, but transcript_path +
/// agent_id locate the sidechain, so the red root it wrote blocks.
#[test]
fn agent_transcript_is_derived_from_transcript_path_and_agent_id() {
    let f = Fixture::new();
    let parent = f.parent_transcript(false);
    let _ = f.sidechain(true);
    let o = f.run(
        "subagent-gate",
        serde_json::json!({
            "session_id": SESSION,
            "transcript_path": parent.to_string_lossy(),
            "agent_id": AGENT_ID,
            "cwd": f.root.to_string_lossy(),
            "hook_event_name": "SubagentStop",
            "stop_hook_active": false,
        }),
    );
    assert_eq!(o.code, Some(0), "{}", dump(&o));
    assert_eq!(o.decision, "block", "{}", dump(&o));
}

/// (5) Chain isolation: the parent's Stop blocking on the same root in the
/// same session (twice, so its own repeat memory is populated) must not make
/// the subagent's FIRST stop a waived repeat.
#[test]
fn parent_block_chain_does_not_waive_the_subagents_first_stop() {
    let f = Fixture::new();
    let parent = f.parent_transcript(true);
    let agent = f.sidechain(true);
    let p1 = f.parent_stop(&parent);
    assert_eq!(p1.decision, "block", "{}", dump(&p1));
    let p2 = f.parent_stop(&parent);
    // Whether the parent's identical repeat is waived is not under test; only
    // that the parent chain has seen this root's finding already.
    let _ = p2;
    let o = f.subagent_stop(&parent, &agent);
    assert_eq!(o.code, Some(0), "{}", dump(&o));
    assert_eq!(
        o.decision,
        "block",
        "the subagent's first stop must block despite the parent's chain. {}",
        dump(&o)
    );
}

/// (6) A subagent that wrote nothing has nothing to judge: no block.
#[test]
fn subagent_that_wrote_nothing_is_not_blocked() {
    let f = Fixture::new();
    let parent = f.parent_transcript(false);
    let agent = f.sidechain(false);
    let o = f.subagent_stop(&parent, &agent);
    assert_eq!(o.code, Some(0), "{}", dump(&o));
    assert_ne!(o.decision, "block", "{}", dump(&o));
}
