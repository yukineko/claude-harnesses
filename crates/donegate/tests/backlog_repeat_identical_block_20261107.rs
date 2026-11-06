// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (2026-11-07, session c2e3bc17): donegate re-blocks the Stop on an
//! IDENTICAL finding — same root, same tree, same failing check, same output —
//! with no new information.
//!
//! Observed: on root ~/.condukt/worktrees/run-20261107-034825-70254-c2-debc6df3
//! donegate blocked the parent's Stop at attempts 1, 2 and 3 (log.jsonl
//! verdict "blocked", 04:33:38 / 04:34:11 / 04:37:00) and gave up at attempt 4
//! (04:37:40), every time on the same `lint-changed: cargo fmt --check FAILED
//! for blastguard` plus the same 2 failing tests; the parent changed nothing in
//! that root in between. User ruling 2026-11-07: 「同一の指摘によって止めてるのは
//! あきらかに無駄」.
//!
//! Why the existing repeat waiver (harness_core::repeat) never fires: donegate's
//! repeat identity `gate::block_fingerprint` deliberately KEEPS the
//! `attempt N/max` counter, so every stop below max_attempts has a new
//! fingerprint and the second-occurrence waiver is unreachable.
//!
//! Desired: when the root's tree and the failing check output are unchanged
//! since the previous block in the same session, the next stop is NOT a
//! `decision:block`; it is not silent either (silence is fail-open, CLAUDE.md
//! §1) — a systemMessage names the still-red check verbatim and says NOTHING
//! NEW WAS VERIFIED. The control pins the other direction: a changed tree with
//! the same red output still blocks.
//!
//! Fixture shape copied from tests/session_worktree_roots.rs (isolated HOME,
//! both roots trusted, HARNESS_TRUST_ALL removed). The failing check writes
//! nothing, so the tree really is byte-identical between the two stops.
//!
//! Written by a disinterested author (CLAUDE.md section 2(a)): no fix exists.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

const RED_CHECK: &str = "wt-check";
const NOTHING_NEW: &str = "NOTHING NEW WAS VERIFIED";

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

struct Stop {
    decision: String,
    system_message: String,
    stdout: String,
    stderr: String,
}

impl Fixture {
    fn new() -> Fixture {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!("dg-repeat-{}-{n}", std::process::id()));
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
        let wt = home.join("linked-wt");
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
        // A deterministic red: identical output on every run, writes nothing.
        std::fs::write(
            wt.join("donegate.toml"),
            format!(
                "max_attempts = 50\n\n[[check]]\nname = \"{RED_CHECK}\"\n\
                 cmd = \"echo 'wt-check: the same red output'; exit 1\"\n"
            ),
        )
        .unwrap();
        std::fs::write(wt.join("session_edit.rs"), "fn edited() {}\n").unwrap();
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

    /// A parent transcript recording one Edit into the linked worktree.
    fn transcript(&self) -> PathBuf {
        let d = self.home.join(".claude/projects/-slug");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("sess.jsonl");
        let mut body = String::from(r#"{"type":"user","message":{"role":"user","content":"go"}}"#);
        body.push('\n');
        body.push_str(
            &serde_json::json!({
                "parentUuid": "p", "isSidechain": false,
                "message": {"model": "claude-opus-5", "id": "m1", "type": "message",
                    "role": "assistant",
                    "content": [{"type": "tool_use", "id": "t1", "name": "Edit",
                        "input": {"file_path": self.wt.join("session_edit.rs").to_string_lossy(),
                                  "old_string": "a", "new_string": "b"}}]}
            })
            .to_string(),
        );
        body.push('\n');
        std::fs::write(&p, body).unwrap();
        p
    }

    fn stop(&self, cwd: &Path, transcript: &Path, stop_hook_active: bool) -> Stop {
        let payload = serde_json::json!({
            "session_id": "s-repeat",
            "transcript_path": transcript.to_string_lossy(),
            "cwd": cwd.to_string_lossy(),
            "hook_event_name": "Stop",
            "stop_hook_active": stop_hook_active,
        })
        .to_string();
        let mut child =
            harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_donegate")))
                .arg("gate")
                .current_dir(cwd)
                .env("HOME", &self.home)
                .env("CLAUDE_CODE_SESSION_ID", "s-repeat")
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
        let v = serde_json::from_str::<serde_json::Value>(stdout.trim()).ok();
        let g = |k: &str| {
            v.as_ref()
                .and_then(|v| v.get(k))
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string()
        };
        Stop {
            decision: g("decision"),
            system_message: g("systemMessage"),
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

/// Two stops on the same, unchanged tree with the same red output.
fn assert_second_identical_stop_does_not_reblock(f: &Fixture, cwd: &Path) {
    let tp = f.transcript();
    let first = f.stop(cwd, &tp, false);
    assert_eq!(
        first.decision, "block",
        "apparatus: the first stop on a red check must block; stdout={:?} stderr={:?}",
        first.stdout, first.stderr
    );
    assert!(
        first.stdout.contains(RED_CHECK),
        "apparatus: the first block must name {RED_CHECK}; stdout={:?}",
        first.stdout
    );
    // Nothing in either root changes between the two stops.
    let second = f.stop(cwd, &tp, true);
    assert_ne!(
        second.decision, "block",
        "the second stop re-blocked on an IDENTICAL finding (same tree, same check, same \
         output) — no new information. stdout={:?} stderr={:?}",
        second.stdout, second.stderr
    );
    assert!(
        second.system_message.contains(RED_CHECK) && second.system_message.contains(NOTHING_NEW),
        "the non-blocking repeat must not be silent: a systemMessage must name the still-red \
         check {RED_CHECK:?} verbatim and say {NOTHING_NEW:?}. stdout={:?}",
        second.stdout
    );
}

/// The observed shape: hook cwd = primary tree, the red root is a linked
/// worktree the session wrote into (the per-root loop in gate_run).
#[test]
fn second_identical_stop_on_extra_root_is_not_a_block_but_says_nothing_new() {
    let f = Fixture::new();
    assert_second_identical_stop_does_not_reblock(&f, &f.primary.clone());
}

/// Same property on the hook-cwd root (the single-root flow in gate_run).
#[test]
fn second_identical_stop_on_cwd_root_is_not_a_block_but_says_nothing_new() {
    let f = Fixture::new();
    assert_second_identical_stop_does_not_reblock(&f, &f.wt.clone());
}

/// CONTROL (passes today, must keep passing): the check output is identical
/// but the TREE changed between the stops — that is new work to verify, so the
/// second stop must still block. Guards a fix that keys on the output alone.
#[test]
fn control_changed_tree_with_same_red_output_still_blocks() {
    let f = Fixture::new();
    let tp = f.transcript();
    let first = f.stop(&f.primary, &tp, false);
    assert_eq!(first.decision, "block", "apparatus: {:?}", first.stdout);
    std::fs::write(f.wt.join("session_edit.rs"), "fn edited_again() {}\n").unwrap();
    let second = f.stop(&f.primary, &tp, true);
    assert_eq!(
        second.decision, "block",
        "a changed tree is new work: the stop must still block even though the red output \
         is identical. stdout={:?} stderr={:?}",
        second.stdout, second.stderr
    );
}
