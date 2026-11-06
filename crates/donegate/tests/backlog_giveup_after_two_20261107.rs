// 丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (user ruling 2026-11-07, backlog b80023b3): 「ブロック回数の断念は2回でダメなら
//! あきらめて別対応すること」.
//!
//! A) built-in default `max_attempts` is 2 (today 3): with no max_attempts in
//!    donegate.toml, stops 1 and 2 block ("attempt 1/2", "attempt 2/2"), stop 3 gives up.
//! B) the give-up is an explicit allow, not a silent one: stdout parses as JSON with a
//!    non-empty `systemMessage` and no `decision:"block"` (presence only; wording is
//!    deliberately not asserted).
//! C) the repo's own donegate.toml must say `max_attempts = 2`.
//!
//! The tree is changed between stops so a separate identical-finding repeat waiver does
//! not interfere. Fixture shape copied from backlog_repeat_identical_block_20261107.rs.
//!
//! Written by a disinterested author (CLAUDE.md section 2(a)): no fix exists.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

const RED_CHECK: &str = "wt-check";

struct Fixture {
    home: PathBuf,
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
        let home = std::env::temp_dir().join(format!("dg-giveup2-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let primary = home.join("primary");
        std::fs::create_dir_all(&primary).unwrap();
        git(&primary, &["init", "-q"]);
        git(&primary, &["config", "user.email", "t@t.com"]);
        git(&primary, &["config", "user.name", "t"]);
        std::fs::write(primary.join("seed.rs"), "fn seed() {}\n").unwrap();
        std::fs::write(
            primary.join("donegate.toml"),
            "[[check]]\nname = \"primary-ok\"\ncmd = \"exit 0\"\n",
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
        // NO max_attempts: the built-in default is what is measured.
        std::fs::write(
            wt.join("donegate.toml"),
            format!(
                "[[check]]\nname = \"{RED_CHECK}\"\n\
                 cmd = \"echo 'wt-check: red output'; exit 1\"\n"
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
        Fixture { home, wt }
    }

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

    fn stop(&self, transcript: &Path, stop_hook_active: bool) -> Stop {
        let cwd = &self.wt;
        let payload = serde_json::json!({
            "session_id": "s-giveup2",
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
                .env("CLAUDE_CODE_SESSION_ID", "s-giveup2")
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

/// Three consecutive stops, the tree changing before each of stops 2 and 3.
fn run_three(f: &Fixture) -> (Stop, Stop, Stop) {
    let tp = f.transcript();
    let edit = f.wt.join("session_edit.rs");
    let s1 = f.stop(&tp, false);
    std::fs::write(&edit, "fn edit_two() {}\n").unwrap();
    let s2 = f.stop(&tp, true);
    std::fs::write(&edit, "fn edit_three() { let _x = 3; }\n").unwrap();
    let s3 = f.stop(&tp, true);
    (s1, s2, s3)
}

/// CONTROL (passes today): stop 1 on a red check blocks and names the check.
#[test]
fn control_first_stop_blocks_and_names_check() {
    let f = Fixture::new();
    let tp = f.transcript();
    let s1 = f.stop(&tp, false);
    assert_eq!(
        s1.decision, "block",
        "stdout={:?} stderr={:?}",
        s1.stdout, s1.stderr
    );
    assert!(s1.stdout.contains(RED_CHECK), "stdout={:?}", s1.stdout);
}

#[test]
fn default_max_attempts_is_two_third_stop_gives_up() {
    let f = Fixture::new();
    let (s1, s2, s3) = run_three(&f);
    assert_eq!(s1.decision, "block", "stop 1: {:?}", s1.stdout);
    assert!(
        s1.stdout.contains("attempt 1/2"),
        "stop 1 must say attempt 1/2: {:?}",
        s1.stdout
    );
    assert_eq!(
        s2.decision, "block",
        "stop 2 must block: {:?} {:?}",
        s2.stdout, s2.stderr
    );
    assert!(
        s2.stdout.contains("attempt 2/2"),
        "stop 2 must say attempt 2/2: {:?}",
        s2.stdout
    );
    assert_ne!(
        s3.decision, "block",
        "stop 3 must give up (default max_attempts = 2), got a block: {:?}",
        s3.stdout
    );
}

#[test]
fn giveup_is_explicit_allow_with_nonempty_system_message() {
    let f = Fixture::new();
    let (_s1, _s2, s3) = run_three(&f);
    assert_ne!(
        s3.decision, "block",
        "apparatus: stop 3 must give up: {:?}",
        s3.stdout
    );
    // Presence only; the wording of the message is deliberately not asserted.
    assert!(
        !s3.system_message.trim().is_empty(),
        "the give-up must be an explicit allow (non-empty JSON systemMessage on stdout), not a \
         silent allow; stdout={:?} stderr={:?}",
        s3.stdout,
        s3.stderr
    );
}

#[test]
fn third_identical_content_stop_is_not_a_block() {
    let f = Fixture::new();
    let tp = f.transcript();
    let _s1 = f.stop(&tp, false);
    let _s2 = f.stop(&tp, true);
    let s3 = f.stop(&tp, true);
    assert_ne!(
        s3.decision, "block",
        "third consecutive stop (tree unchanged) must not block; stdout={:?} stderr={:?}",
        s3.stdout, s3.stderr
    );
}

#[test]
fn repo_donegate_toml_max_attempts_is_two() {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../donegate.toml");
    let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
    let v: toml::Value = text
        .parse()
        .unwrap_or_else(|e| panic!("parse {}: {e}", p.display()));
    let got = v.get("max_attempts").and_then(|x| x.as_integer());
    assert_eq!(
        got,
        Some(2),
        "repo donegate.toml max_attempts must be 2, got {got:?}"
    );
}
