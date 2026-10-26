#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Spec: implementation changes the session COMMITTED during the session (in
//! any root) are evaluated like uncommitted ones. Observed defect: the session
//! wrote impl + test in a linked worktree, committed, and stopped with the hook
//! cwd in the primary tree; tdd scanned only uncommitted diffs, evaluated
//! nothing, and blocked ("an empty scan is not a pass") a test-first session.
//!
//! `committed_impl_with_test_is_accepted` is the REPRO (fails until the fix).
//! `committed_impl_without_test_still_blocks` is the anti-vacuity control: a
//! fix that allows whenever the scan is empty would fail it.
//!
//! Isolation: `$HOME` is a per-test tempdir (trust list, transcript); the real
//! `~/.claude` is never read. Written by a disinterested author (CLAUDE.md 2(a)).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

const IMPL: &str =
    "pub fn f() -> i32 {\n    let x = 1;\n    let y = x + 1;\n    println!(\"{}\", y);\n    y\n}\n";
const TEST: &str = "#[test]\nfn f_works() {\n    assert_eq!(2, 2);\n}\n";

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

impl Fixture {
    fn new() -> Fixture {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!("tdd-committed-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let primary = home.join("primary");
        std::fs::create_dir_all(&primary).unwrap();
        git(&primary, &["init", "-q"]);
        git(&primary, &["config", "user.email", "t@t.com"]);
        git(&primary, &["config", "user.name", "t"]);
        git(&primary, &["config", "commit.gpgsign", "false"]);
        std::fs::write(primary.join("seed.txt"), "seed\n").unwrap();
        git(&primary, &["add", "seed.txt"]);
        git(&primary, &["commit", "-qm", "init"]);
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

    /// Write `rel` in the worktree (creating dirs); returns its absolute path.
    fn put(&self, rel: &str, body: &str) -> PathBuf {
        let p = self.wt.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        p
    }

    fn commit_all(&self) {
        git(&self.wt, &["add", "-A"]);
        git(&self.wt, &["commit", "-qm", "session work"]);
    }

    fn transcript(&self, written: &[&Path]) -> PathBuf {
        let d = self.home.join(".claude/projects/-slug");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("sess.jsonl");
        let mut body = String::from(r#"{"type":"user","message":{"role":"user","content":"go"}}"#);
        body.push('\n');
        for path in written {
            body.push_str(
                &serde_json::json!({
                    "parentUuid": "p", "isSidechain": false,
                    "message": {"model": "claude-opus-5", "id": "m1", "type": "message",
                        "role": "assistant",
                        "content": [{"type": "tool_use", "id": "t1", "name": "Write",
                            "input": {"file_path": path.to_string_lossy(), "content": "x"}}]}
                })
                .to_string(),
            );
            body.push('\n');
        }
        std::fs::write(&p, body).unwrap();
        p
    }

    /// Run the real `tdd gate` with cwd = primary; returns (stdout, stderr).
    fn stop(&self, transcript: &Path) -> (String, String) {
        let payload = serde_json::json!({
            "session_id": "s-committed",
            "transcript_path": transcript.to_string_lossy(),
            "cwd": self.primary.to_string_lossy(),
            "hook_event_name": "Stop",
            "stop_hook_active": false,
        })
        .to_string();
        let mut child = Command::new(env!("CARGO_BIN_EXE_tdd"))
            .arg("gate")
            .current_dir(&self.primary)
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_SESSION_ID", "s-committed")
            .env_remove("TDD_DISABLE")
            .env_remove("HARNESS_TRUST_ALL")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("tdd spawns");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(0), "Stop hook must exit 0");
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn blocks(stdout: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(stdout.trim())
        .ok()
        .and_then(|v| {
            v.get("decision")
                .and_then(|d| d.as_str().map(str::to_string))
        })
        .as_deref()
        == Some("block")
}

/// REPRO: impl + test written and committed in the worktree, tree clean, hook
/// cwd = primary. The committed work is test-accompanied and must be accepted.
#[test]
#[ignore = "backlog: tdd ignores committed session work — RED repro"]
fn committed_impl_with_test_is_accepted() {
    let f = Fixture::new();
    let imp = f.put("src/lib.rs", IMPL);
    let tst = f.put("tests/it.rs", TEST);
    f.commit_all();
    let tp = f.transcript(&[&imp, &tst]);
    let (stdout, stderr) = f.stop(&tp);
    assert!(
        !blocks(&stdout),
        "committed impl+test must be evaluated and accepted; stdout={stdout:?} stderr={stderr:?}"
    );
}

/// ANTI-VACUITY: committed impl with NO test must still block, so the fix
/// cannot be "allow whenever the scan is empty".
#[test]
fn committed_impl_without_test_still_blocks() {
    let f = Fixture::new();
    let imp = f.put("src/lib.rs", IMPL);
    f.commit_all();
    let tp = f.transcript(&[&imp]);
    let (stdout, stderr) = f.stop(&tp);
    assert!(
        blocks(&stdout),
        "committed untested impl must block; stdout={stdout:?} stderr={stderr:?}"
    );
}
