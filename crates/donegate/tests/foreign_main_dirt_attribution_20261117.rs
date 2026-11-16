// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (2026-11-17): donegate's Stop gate runs (and blocks on) its
//! `when_changed` checks for crate changes ANOTHER session left transiently
//! dirty in main, although THIS session changed nothing.
//!
//! Observed (session 1e000416, 2026-11-17): at Stop, donegate ran
//! lint-changed ("cargo fmt --check FAILED for condukt") and test-changed
//! (condukt `audit_b12_oracle_ids_and_teardown`) against condukt changes that
//! were in main only transiently (another session's); this session never
//! touched condukt and `git status --short` on main was empty right after.
//!
//! Path (read from the code at 7767e6a8f):
//!   * crates/donegate/src/gate.rs:133 `scan_scope(crate::git::changed_files(root))`
//!     — the scope of every `when_changed` check is the TREE's change set;
//!   * crates/donegate/src/git.rs:41 `changed_files` = `git diff --name-only`
//!     + `--cached` + untracked + merge-base(main)..HEAD. No session input;
//!   * scripts/test-changed-crates.sh / scripts/lint-changed-crates.sh pick the
//!     crates from the same tree dirt (`git diff --name-only HEAD`, untracked,
//!     merge-base..HEAD) and take no session input either.
//!
//! donegate.toml's own header states the intent "a red crate the current
//! turn never touched blocked the stop of whoever WAS working" is what the
//! scoping is meant to remove, and gate.rs:367-372 says the checks "routinely
//! fail on ANOTHER session's changes (CLAUDE.md §8's measured attribution
//! bug)". CLAUDE.md §8 calls such a demand "帰属のバグ".
//!
//! What this pins: hook cwd = main, the session transcript records only a
//! read-only Bash call, main holds another writer's dirty `.rs` under a crate
//! -> the stop must not be blocked by a check scoped to that file. Control:
//! the same dirt, with the transcript recording this session's Edit of it,
//! still blocks.
//!
//! DOES NOT PROVE: which attribution source a fix should use (the transcript,
//! the maintree-writes record, ...); the control passes today only because
//! donegate blocks on any tree dirt.
//!
//! Written by an agent that did not implement donegate (CLAUDE.md 2(a)).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);
const MARKER: &str = "check-ran.marker";
const FOREIGN: &str = "crates/condukt/src/main.rs";

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

struct Fixture {
    home: PathBuf,
    main: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!("dg-foreign-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let main = home.join("main");
        std::fs::create_dir_all(main.join("crates/condukt/src")).unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        git(&main, &["config", "user.email", "t@t.com"]);
        git(&main, &["config", "user.name", "t"]);
        std::fs::write(main.join(FOREIGN), "fn main() {}\n").unwrap();
        // The marker goes outside the tree so it is not itself a change.
        let marker = home.join(MARKER);
        std::fs::write(
            main.join("donegate.toml"),
            format!(
                "max_attempts = 50\n\n[[check]]\nname = \"test-changed\"\n\
                 cmd = \"echo ran > {}; exit 1\"\nwhen_changed = [\"**/*.rs\"]\n",
                marker.display()
            ),
        )
        .unwrap();
        git(&main, &["add", "-A"]);
        git(
            &main,
            &["-c", "commit.gpgsign=false", "commit", "-qm", "init"],
        );
        let main = main.canonicalize().unwrap();
        std::fs::create_dir_all(home.join(".harness")).unwrap();
        std::fs::write(
            home.join(".harness/trust.toml"),
            format!("trusted = [\"{}\"]\n", main.display()),
        )
        .unwrap();
        Fixture { home, main }
    }

    fn marker(&self) -> PathBuf {
        self.home.join(MARKER)
    }

    /// Another session's in-flight edit of a condukt source file in main.
    fn foreign_dirt(&self) {
        std::fs::write(self.main.join(FOREIGN), "fn main() { let x=1; }\n").unwrap();
    }

    fn transcript(&self, uses: &[(&str, serde_json::Value)]) -> PathBuf {
        let d = self.home.join(".claude/projects/-slug");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("sess.jsonl");
        let mut body = String::from(r#"{"type":"user","message":{"role":"user","content":"go"}}"#);
        body.push('\n');
        for (i, (tool, input)) in uses.iter().enumerate() {
            body.push_str(
                &serde_json::json!({
                    "parentUuid": "p", "isSidechain": false,
                    "message": {"model": "claude-opus-5", "id": format!("m{i}"), "type": "message",
                        "role": "assistant",
                        "content": [{"type": "tool_use", "id": format!("t{i}"), "name": tool, "input": input}]}
                })
                .to_string(),
            );
            body.push('\n');
        }
        std::fs::write(&p, body).unwrap();
        p
    }

    /// Run `donegate gate` as the Stop hook with cwd = main. (decision, reason, stderr)
    fn stop(&self, transcript: &Path) -> (String, String, String) {
        let payload = serde_json::json!({
            "session_id": "s-foreign",
            "transcript_path": transcript.to_string_lossy(),
            "cwd": self.main.to_string_lossy(),
            "hook_event_name": "Stop",
            "stop_hook_active": false,
        })
        .to_string();
        let mut child =
            harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_donegate")))
                .arg("gate")
                .current_dir(&self.main)
                .env("HOME", &self.home)
                .env("CLAUDE_CODE_SESSION_ID", "s-foreign")
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
        (
            g("decision"),
            g("reason"),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn bash(cmd: &str) -> (&'static str, serde_json::Value) {
    ("Bash", serde_json::json!({"command": cmd}))
}

fn edit(path: &Path) -> (&'static str, serde_json::Value) {
    (
        "Edit",
        serde_json::json!({"file_path": path.to_string_lossy(), "old_string": "a", "new_string": "b"}),
    )
}

/// RED: this session ran only `git status`; the dirty condukt file is another
/// session's. donegate must not run the crate check for it and block.
#[test]
#[ignore = "RED 2026-11-17: donegate scopes when_changed checks to tree dirt, not to this session's writes; remove when fixed"]
fn readonly_session_is_not_blocked_by_foreign_crate_dirt_in_main() {
    let f = Fixture::new();
    f.foreign_dirt();
    let tp = f.transcript(&[bash("git status"), bash("git log --oneline -5")]);
    let (decision, reason, err) = f.stop(&tp);
    assert!(
        decision != "block" && !f.marker().exists(),
        "donegate ran its crate check on ANOTHER session's dirt and blocked a session \
         that changed nothing: decision={decision:?} marker_exists={} reason={reason:?} err={err:?}",
        f.marker().exists()
    );
}

/// CONTROL: the same dirt, recorded as this session's own Edit, still runs the
/// check and blocks.
#[test]
fn control_own_edit_of_the_crate_still_runs_the_check_and_blocks() {
    let f = Fixture::new();
    f.foreign_dirt();
    let tp = f.transcript(&[edit(&f.main.join(FOREIGN))]);
    let (decision, reason, err) = f.stop(&tp);
    assert!(
        f.marker().exists(),
        "apparatus: the check must run for this session's own change; err={err:?}"
    );
    assert_eq!(decision, "block", "reason={reason:?} err={err:?}");
}
