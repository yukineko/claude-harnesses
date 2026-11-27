// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (2026-11-17, measured at rev cd09054b1): donegate judges the
//! PARENT session's worktree root for work a subagent did in ANOTHER worktree.
//!
//! Observed (session df7c968f, 2026-11-17): the main session's cwd was
//! `~/src/.harness-worktrees/session-df7c968f`. A worker subagent changed code
//! in a different worktree (`/mnt/c/tmp/aegis-worktrees/run-...-t2`) and
//! stopped; donegate's test-changed check then ran in the PARENT's worktree
//! root (demanded the parent's changes/tests, ran the lock test there and
//! failed). CLAUDE.md section 8: a gate claiming another session's work as
//! yours is an attribution bug.
//!
//! Why the parent root is what got judged: donegate judges ONLY the hook cwd
//! (`crates/donegate/src/rootcheck.rs`, `subagent.rs`, `main.rs::gate_run`:
//! `let root = input.cwd_or_current();`). So the observed judged root WAS the
//! hook cwd, i.e. the hook cwd was the parent root. It is judged when a write
//! of the session/subagent lies "inside the cwd" (`rootcheck::writes_in`).
//!
//! Suspected mechanism (to be confirmed by this test): a subagent's shell
//! cwd is reset to the session cwd between Bash calls, so it writes to its own
//! worktree with `cd <wt> && <write> <relative path>`. The transcript line
//! records `cwd` = parent root. `harness_core::attribution::own_written_paths`
//! joins the relative word with that recorded line cwd, IGNORING the `cd`
//! (contrary to the `writing_statements` doc: "the directory it enters is
//! where that later statement's relative paths land"). Because every worktree
//! of a repo has the same layout, `<parent root>/<relative path>` exists, so
//! the parent root is taken as written and its checks run.
//!
//! Two readings and how to tell them apart:
//!  (A) The subagent really wrote inside the parent root (then judging it is
//!      the 2026-11-08 contract, not a bug). In this fixture the subagent's
//!      only write is `cd <wt> && sed -i ... src/lib.rs`; the parent's
//!      src/lib.rs is never modified, so (A) does not hold here.
//!  (B) Attribution places the relative path in the wrong directory. The
//!      parent check carries a marker (`ran-parent`): if it exists after the
//!      stop, donegate ran the parent's checks for the subagent's wt write.
//!  The control `subagent_stop_with_cwd_in_the_worktree_judges_the_worktree`
//!  shows the wt is judged when the hook cwd IS the wt, isolating the defect
//!  to the cwd-relative join rather than to the root selection.
//!
//! What this test does NOT prove:
//!  - That Claude Code's real SubagentStop PAYLOAD `cwd` is the parent root.
//!    Not read directly; inferred from donegate's judge-only-the-cwd contract
//!    plus the observation that the parent root was judged.
//!    What IS observed (2026-11-17, main session re-ran blastguard relay
//!    bgr-108f07d82bfd over the real sidechains of session df7c968f): every
//!    sidechain (`isSidechain: true`) Bash line records `cwd` =
//!    `/home/hiroyuki_nakayama/src/.harness-worktrees/session-df7c968f` (the
//!    parent's worktree), and the commands reach the worker worktree as
//!    `cd /mnt/c/tmp/aegis-worktrees/run-...-t1 && ...` /
//!    `cd /mnt/c/tmp/aegis-worktrees/run-20261117-100335-22251-t2 && ...`.
//!    That is the transcript shape this fixture builds.
//!  - That the `...` after the observed `cd` contained a relative-path write
//!    whose path also exists in the parent root (the exact command bodies were
//!    not inspected). Other spellings (e.g. `git -C <wt> ...`) are not covered.
//!  - What the fix should judge instead (the wt must NOT be judged from a
//!    parent-root hook either, per the 2026-11-08 ruling); the RED asserts
//!    only that the parent root's checks do not run.
//!
//! Written by a disinterested author (CLAUDE.md section 2(a)): no fix exists.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

const AGENT_ID: &str = "a20261117cdrel0001";
const SESSION: &str = "s-cdrel-20261117";

struct Fixture {
    home: PathBuf,
    /// The parent session's worktree root (the hook cwd).
    parent: PathBuf,
    /// The subagent's own worktree (same layout as `parent`).
    wt: PathBuf,
}

struct Outcome {
    code: Option<i32>,
    decision: String,
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

fn user_line() -> String {
    line(serde_json::json!({
        "type": "user", "message": {"role": "user", "content": "go"}
    }))
}

/// One Bash tool_use transcript line, recording `cwd` like Claude Code does.
fn bash_use(id: &str, command: &str, cwd: &Path, sidechain: bool) -> String {
    line(serde_json::json!({
        "parentUuid": "p", "isSidechain": sidechain, "cwd": cwd.to_string_lossy(),
        "message": {"model": "claude-opus-5", "id": format!("m-{id}"), "type": "message",
            "role": "assistant",
            "content": [{"type": "tool_use", "id": id, "name": "Bash",
                "input": {"command": command}}]}
    }))
}

/// A git repo with `src/lib.rs` and a red check that drops `marker` in `home`
/// when it runs.
fn make_repo(home: &Path, name: &str, marker: &str) -> PathBuf {
    let root = home.join(name);
    std::fs::create_dir_all(root.join("src")).unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["config", "user.email", "t@t.com"]);
    git(&root, &["config", "user.name", "t"]);
    std::fs::write(root.join("src/lib.rs"), "fn a() {}\n").unwrap();
    let root = root.canonicalize().unwrap();
    std::fs::write(
        root.join("donegate.toml"),
        format!(
            "max_attempts = 50\n\n[[check]]\nname = \"marker-check\"\n\
             cmd = \"touch {}; echo 'marker check is red'; false\"\n",
            home.join(marker).display()
        ),
    )
    .unwrap();
    git(&root, &["add", "src/lib.rs", "donegate.toml"]);
    git(
        &root,
        &["-c", "commit.gpgsign=false", "commit", "-qm", "init"],
    );
    root
}

impl Fixture {
    fn new() -> Fixture {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!("dg-cdrel-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let home = home.canonicalize().unwrap();
        let parent = make_repo(&home, "parent", "ran-parent");
        let wt = make_repo(&home, "wt", "ran-wt");
        std::fs::create_dir_all(home.join(".harness")).unwrap();
        std::fs::write(
            home.join(".harness/trust.toml"),
            format!(
                "trusted = [\"{}\", \"{}\"]\n",
                parent.display(),
                wt.display()
            ),
        )
        .unwrap();
        Fixture { home, parent, wt }
    }

    fn ran(&self, marker: &str) -> bool {
        self.home.join(marker).exists()
    }

    fn project_dir(&self) -> PathBuf {
        let d = self.home.join(".claude/projects/-slug");
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The parent transcript; `body` lines after the user prompt.
    fn parent_transcript(&self, body: &str) -> PathBuf {
        let p = self.project_dir().join("sess.jsonl");
        std::fs::write(&p, format!("{}{body}", user_line())).unwrap();
        p
    }

    /// The subagent writes ONLY into its own worktree, the way an agent whose
    /// shell cwd is reset to the session cwd must: `cd <wt> && sed -i ... <rel>`.
    /// The line's recorded `cwd` is the parent root (the session cwd).
    fn sidechain_writing_wt_via_cd(&self) -> PathBuf {
        let dir = self.project_dir().join("sess").join("subagents");
        std::fs::create_dir_all(&dir).unwrap();
        let cmd = format!("cd {} && sed -i s/a/b/ src/lib.rs", self.wt.display());
        let body = format!(
            "{}{}",
            user_line(),
            bash_use("t-sub", &cmd, &self.parent, true)
        );
        let p = dir.join(format!("agent-{AGENT_ID}.jsonl"));
        std::fs::write(&p, body).unwrap();
        p
    }

    fn run(&self, sub: &str, cwd: &Path, payload: serde_json::Value) -> Outcome {
        let mut child =
            harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_donegate")))
                .arg(sub)
                .current_dir(cwd)
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
        let decision = v
            .as_ref()
            .and_then(|v| v.get("decision"))
            .and_then(|d| d.as_str())
            .map(String::from)
            .unwrap_or_default();
        Outcome {
            code: out.status.code(),
            decision,
            stdout,
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn subagent_stop(&self, cwd: &Path, parent_t: &Path, agent_t: &Path) -> Outcome {
        self.run(
            "subagent-gate",
            cwd,
            serde_json::json!({
                "session_id": SESSION,
                "transcript_path": parent_t.to_string_lossy(),
                "agent_transcript_path": agent_t.to_string_lossy(),
                "agent_id": AGENT_ID,
                "cwd": cwd.to_string_lossy(),
                "hook_event_name": "SubagentStop",
                "stop_hook_active": false,
            }),
        )
    }

    fn parent_stop(&self, parent_t: &Path) -> Outcome {
        self.run(
            "gate",
            &self.parent,
            serde_json::json!({
                "session_id": SESSION,
                "transcript_path": parent_t.to_string_lossy(),
                "cwd": self.parent.to_string_lossy(),
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
    format!(
        "code={:?} stdout={:?} stderr={:?}",
        o.code, o.stdout, o.stderr
    )
}

/// RED: the subagent wrote only into its own worktree (`cd <wt> && sed -i
/// src/lib.rs`), yet its SubagentStop (hook cwd = parent root) runs the
/// PARENT root's checks and blocks the subagent on them.
// test-weakening-justified: new RED for an unfixed defect (2026-11-17).
#[test]
fn subagent_stop_does_not_judge_parent_root_for_a_cd_relative_write_in_its_worktree() {
    let f = Fixture::new();
    let parent_t = f.parent_transcript("");
    let agent_t = f.sidechain_writing_wt_via_cd();
    let o = f.subagent_stop(&f.parent.clone(), &parent_t, &agent_t);
    let parent_lib = std::fs::read_to_string(f.parent.join("src/lib.rs")).unwrap();
    assert_eq!(
        parent_lib, "fn a() {}\n",
        "fixture: the parent root is untouched"
    );
    assert!(
        !f.ran("ran-parent"),
        "the parent root's checks ran for a write the subagent made in {}: {}",
        f.wt.display(),
        dump(&o)
    );
    assert_ne!(o.decision, "block", "{}", dump(&o));
}

/// RED: same subagent work, at the PARENT's Stop (whose writes include every
/// sidechain): the parent root, which nobody wrote into, is judged and blocks.
// test-weakening-justified: new RED for an unfixed defect (2026-11-17).
#[test]
#[ignore = "RED(49ec9e90): parent Stop judges hook cwd whenever checks are configured (main.rs:276); fix needs a human ruling on skipping checks when wrote_in_cwd is Known(false)"]
fn parent_stop_does_not_judge_parent_root_for_a_subagents_cd_relative_write_elsewhere() {
    let f = Fixture::new();
    let parent_t = f.parent_transcript("");
    let _agent_t = f.sidechain_writing_wt_via_cd();
    let o = f.parent_stop(&parent_t);
    assert!(
        !f.ran("ran-parent"),
        "the parent root's checks ran for a write the subagent made in {}: {}",
        f.wt.display(),
        dump(&o)
    );
    assert_ne!(o.decision, "block", "{}", dump(&o));
}

/// Control: a main-session Stop where the session itself wrote into its cwd
/// (relative path, no `cd`) is judged as before: the check runs and blocks.
#[test]
fn control_parent_stop_with_own_write_in_cwd_is_judged() {
    let f = Fixture::new();
    let body = bash_use("t-par", "sed -i s/a/b/ src/lib.rs", &f.parent, false);
    let parent_t = f.parent_transcript(&body);
    let o = f.parent_stop(&parent_t);
    assert!(f.ran("ran-parent"), "{}", dump(&o));
    assert_eq!(o.code, Some(0), "{}", dump(&o));
    assert_eq!(o.decision, "block", "{}", dump(&o));
}

/// Control: if the SubagentStop hook cwd IS the subagent's worktree, the wt
/// (not the parent) is judged. Separates "wrong root chosen" from "relative
/// path joined to the wrong directory".
#[test]
fn control_subagent_stop_with_cwd_in_the_worktree_judges_the_worktree() {
    let f = Fixture::new();
    let parent_t = f.parent_transcript("");
    let agent_t = f.sidechain_writing_wt_via_cd();
    let o = f.subagent_stop(&f.wt.clone(), &parent_t, &agent_t);
    assert!(f.ran("ran-wt"), "{}", dump(&o));
    assert!(!f.ran("ran-parent"), "{}", dump(&o));
    assert_eq!(o.decision, "block", "{}", dump(&o));
}
