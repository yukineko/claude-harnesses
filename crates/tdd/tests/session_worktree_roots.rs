// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED reproduction: the session worked in a LINKED git worktree while the
//! Stop-hook `cwd` was the primary tree. tdd uses `input.cwd_or_current()` as
//! its only root, so it evaluated a clean tree and allowed, never looking at
//! the implementation file the session wrote in the worktree.
//!
//! Intended contract: roots = hook cwd + git toplevel of every file the
//! session transcript records as written; evaluated per root. If the session
//! wrote files but nothing was evaluated, or a written root cannot be resolved
//! (path gone / not a git repo), the stop BLOCKS with a reason naming the
//! roots. A session with no recorded writes still allows.
//!
//! Fixture: `$HOME` is a tempdir with a trust list naming BOTH trees;
//! `HARNESS_TRUST_ALL` removed. `control_cwd_is_the_worktree_blocks_the_impl_file`
//! shows tdd does block the worktree's untested impl file when it is the cwd
//! (anti-vacuity).
//!
//! Written by a disinterested author (CLAUDE.md section 2(a)).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

const BODY: &str = "fn f() {\n    let x = 1;\n    println!(\"{}\", x);\n}\n";

struct Fixture {
    home: PathBuf,
    primary: PathBuf,
    wt: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let st = Command::new("git")
        .current_dir(dir)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git runs");
    assert!(st.success(), "git {args:?} failed in {}", dir.display());
}

struct Outcome {
    decision: String,
    reason: String,
    stdout: String,
    stderr: String,
}

impl Fixture {
    fn new() -> Fixture {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!("tdd-wtroots-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let primary = home.join("primary");
        std::fs::create_dir_all(&primary).unwrap();
        git(&primary, &["init", "-q"]);
        git(&primary, &["config", "user.email", "t@t.com"]);
        git(&primary, &["config", "user.name", "t"]);
        std::fs::write(primary.join("seed.rs"), "fn seed() {}\n").unwrap();
        git(&primary, &["add", "seed.rs"]);
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
        std::fs::write(wt.join("session_edit.rs"), BODY).unwrap();

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

    fn transcript(&self, uses: &[(&str, serde_json::Value)]) -> PathBuf {
        let d = self.home.join(".claude/projects/-slug");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("sess.jsonl");
        let mut body = String::from(r#"{"type":"user","message":{"role":"user","content":"go"}}"#);
        body.push('\n');
        for (tool, input) in uses {
            body.push_str(
                &serde_json::json!({
                    "parentUuid": "p", "isSidechain": false,
                    "message": {"model": "claude-opus-5", "id": "m1", "type": "message",
                        "role": "assistant",
                        "content": [{"type": "tool_use", "id": "t1", "name": tool, "input": input}]}
                })
                .to_string(),
            );
            body.push('\n');
        }
        std::fs::write(&p, body).unwrap();
        p
    }

    fn stop(&self, cwd: &Path, transcript: &Path) -> Outcome {
        let payload = serde_json::json!({
            "session_id": "s-wtroots",
            "transcript_path": transcript.to_string_lossy(),
            "cwd": cwd.to_string_lossy(),
            "hook_event_name": "Stop",
            "stop_hook_active": false,
        })
        .to_string();
        let mut child = Command::new(env!("CARGO_BIN_EXE_tdd"))
            .arg("gate")
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_SESSION_ID", "s-wtroots")
            .env_remove("TDD_DISABLE")
            .env_remove("HARNESS_TRUST_ALL")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("reviewgate spawns");
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
        Outcome {
            decision: g("decision"),
            reason: g("reason"),
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

fn edit(path: &Path) -> (&'static str, serde_json::Value) {
    (
        "Edit",
        serde_json::json!({"file_path": path.to_string_lossy(), "old_string": "a", "new_string": "b"}),
    )
}

fn read(path: &Path) -> (&'static str, serde_json::Value) {
    (
        "Read",
        serde_json::json!({"file_path": path.to_string_lossy()}),
    )
}

/// ANTI-VACUITY CONTROL: cwd = the worktree blocks on its untested impl file,
/// so the RED test below cannot be an untrusted/disabled allow.
#[test]
fn control_cwd_is_the_worktree_blocks_the_impl_file() {
    let f = Fixture::new();
    let tp = f.transcript(&[edit(&f.wt.join("session_edit.rs"))]);
    let o = f.stop(&f.wt, &tp);
    assert_eq!(
        o.decision, "block",
        "apparatus: {} / {}",
        o.stdout, o.stderr
    );
}

/// THE BUG. cwd = primary tree, Edit names the worktree impl file: tdd must
/// evaluate it (block: impl without a test) and the reason names the file.
#[test]
fn worktree_impl_file_is_evaluated_when_cwd_is_the_primary_tree() {
    let f = Fixture::new();
    let tp = f.transcript(&[edit(&f.wt.join("session_edit.rs"))]);
    let o = f.stop(&f.primary, &tp);
    assert_eq!(
        o.decision, "block",
        "tdd judged the primary tree only; stdout={:?} stderr={:?}",
        o.stdout, o.stderr
    );
    assert!(
        o.reason.contains("session_edit.rs"),
        "reason must name the worktree file; reason={:?}",
        o.reason
    );
}

/// The session wrote files (recorded Edit of a clean worktree file: nothing
/// evaluated). Must block, naming the roots.
#[test]
fn wrote_files_but_nothing_evaluated_blocks_naming_roots() {
    let f = Fixture::new();
    let tp = f.transcript(&[edit(&f.wt.join("seed.rs"))]);
    let o = f.stop(&f.primary, &tp);
    assert_eq!(
        o.decision, "block",
        "session wrote files, nothing evaluated: must block. stdout={:?} stderr={:?}",
        o.stdout, o.stderr
    );
    assert!(
        o.reason.contains(&f.wt.display().to_string())
            && o.reason.contains(&f.primary.display().to_string()),
        "reason must name both roots; reason={:?}",
        o.reason
    );
}

#[test]
fn written_root_gone_blocks_naming_the_root() {
    let f = Fixture::new();
    let gone = f.home.join("gone-root-4711").join("x.rs");
    let tp = f.transcript(&[edit(&gone)]);
    let o = f.stop(&f.primary, &tp);
    assert_eq!(
        o.decision, "block",
        "unresolvable written root must block. stdout={:?} stderr={:?}",
        o.stdout, o.stderr
    );
    assert!(
        o.reason.contains("gone-root-4711"),
        "reason must name the root; reason={:?}",
        o.reason
    );
}

#[test]
fn written_root_not_a_git_repo_blocks_naming_the_root() {
    let f = Fixture::new();
    let plain = f.home.join("plain-dir-4712");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join("x.rs"), BODY).unwrap();
    let tp = f.transcript(&[edit(&plain.join("x.rs"))]);
    let o = f.stop(&f.primary, &tp);
    assert_eq!(
        o.decision, "block",
        "non-git written root must block. stdout={:?} stderr={:?}",
        o.stdout, o.stderr
    );
    assert!(
        o.reason.contains("plain-dir-4712"),
        "reason must name the root; reason={:?}",
        o.reason
    );
}

/// CONTROL: no recorded writes (only a Read) still allows. The clean primary
/// tree plus a worktree impl file the session never wrote must not block.
#[test]
fn control_session_with_no_writes_still_allows() {
    let f = Fixture::new();
    let tp = f.transcript(&[read(&f.wt.join("session_edit.rs"))]);
    let o = f.stop(&f.primary, &tp);
    assert_ne!(o.decision, "block", "no writes must allow; {:?}", o.reason);
}
