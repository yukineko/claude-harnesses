// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! INTENTIONALLY RED repro (backlog 355d3b12, filed 2026-11-07).
//!
//! `git::changed_files` only sees UNCOMMITTED changes (`git diff`, `git diff
//! --cached`, `ls-files --others`). In a linked worktree whose branch carries
//! COMMITTED work not yet on `main`, with a clean tree, a check declared with
//! `when_changed = ["**/*.rs"]` is therefore skipped and the Stop gate allows.
//!
//! Entry point: the real `donegate gate` Stop hook (black box). The gated check
//! is `exit 1`, so the observable is the Stop verdict: `decision == "block"`
//! means the check ran (not skipped); anything else means it was skipped/allowed.
//!
//! Tests:
//!  * control (passes today): the same .rs change left UNCOMMITTED is detected,
//!    so the check runs and the gate blocks. Proves the fixture/config are live.
//!  * RED 1: committed-unmerged .rs change (clean tree, `git diff main...HEAD`
//!    names it) must make the check run.
//!  * RED 2: when the merge-base with `main` cannot be determined (no `main`
//!    branch exists), the changed set must not read as "no changes": the check
//!    must still run (fail-closed).
//!
//! Written by a disinterested author (CLAUDE.md section 2(a)).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

const TOML: &str = r#"
[[check]]
name = "rs-gate"
cmd = "exit 1"
when_changed = ["**/*.rs"]
"#;

struct Fixture {
    home: PathBuf,
    wt: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let st = harness_core::git_env::scrub(&mut Command::new("git"))
        .current_dir(dir)
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git runs");
    assert!(st.success(), "git {args:?} failed in {}", dir.display());
}

enum Change {
    Committed,
    Uncommitted,
}

impl Fixture {
    /// `base`: name of the primary repo's initial branch ("main" or "trunk").
    fn new(base: &str, change: Change) -> Fixture {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!("dg-cu-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let primary = home.join("primary");
        std::fs::create_dir_all(&primary).unwrap();
        git(&primary, &["init", "-q", "-b", base]);
        git(&primary, &["config", "user.email", "t@t.com"]);
        git(&primary, &["config", "user.name", "t"]);
        std::fs::write(primary.join("README.md"), "seed\n").unwrap();
        std::fs::write(primary.join("donegate.toml"), TOML).unwrap();
        git(&primary, &["add", "README.md", "donegate.toml"]);
        git(&primary, &["commit", "-qm", "init"]);
        let wt = home.join("task-wt");
        git(
            &primary,
            &["worktree", "add", "-q", "-b", "task", wt.to_str().unwrap()],
        );
        std::fs::create_dir_all(wt.join("src")).unwrap();
        std::fs::write(wt.join("src/feature.rs"), "pub fn f() {}\n").unwrap();
        if let Change::Committed = change {
            git(&wt, &["add", "src/feature.rs"]);
            git(&wt, &["commit", "-qm", "feature"]);
        }
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

    /// Run the Stop hook in the worktree; return (decision, stdout, stderr).
    /// `decision` is "" when the hook printed no decision.
    fn stop_decision(&self) -> (String, String, String) {
        let d = self.home.join(".claude/projects/-slug");
        std::fs::create_dir_all(&d).unwrap();
        let tp = d.join("sess.jsonl");
        let mut body = String::from(r#"{"type":"user","message":{"role":"user","content":"go"}}"#);
        body.push('\n');
        body.push_str(
            &serde_json::json!({
                "parentUuid": "p", "isSidechain": false,
                "message": {"model": "claude-opus-5", "id": "m1", "type": "message",
                    "role": "assistant",
                    "content": [{"type": "tool_use", "id": "t1", "name": "Edit",
                        "input": {"file_path": self.wt.join("src/feature.rs").to_string_lossy(),
                                  "old_string": "a", "new_string": "b"}}]}
            })
            .to_string(),
        );
        body.push('\n');
        std::fs::write(&tp, body).unwrap();
        let payload = serde_json::json!({
            "session_id": "s-cu",
            "transcript_path": tp.to_string_lossy(),
            "cwd": self.wt.to_string_lossy(),
            "hook_event_name": "Stop",
            "stop_hook_active": false,
        })
        .to_string();
        let mut child =
            harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_donegate")))
                .arg("gate")
                .current_dir(&self.wt)
                .env("HOME", &self.home)
                .env("CLAUDE_CODE_SESSION_ID", "s-cu")
                .env_remove("DONEGATE_DISABLE")
                .env_remove("HARNESS_TRUST_ALL")
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
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let decision = stdout
            .lines()
            .rev()
            .find_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
            .and_then(|v| v.get("decision").and_then(|d| d.as_str().map(String::from)))
            .unwrap_or_default();
        (decision, stdout, stderr)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

#[test]
fn control_uncommitted_rs_change_runs_when_changed_check() {
    let f = Fixture::new("main", Change::Uncommitted);
    let (decision, stdout, stderr) = f.stop_decision();
    assert_eq!(
        decision, "block",
        "uncommitted .rs change must run the gated check; stdout={stdout:?} stderr={stderr:?}"
    );
}

#[test]
fn committed_unmerged_rs_change_runs_when_changed_check() {
    let f = Fixture::new("main", Change::Committed);
    let (decision, stdout, stderr) = f.stop_decision();
    assert_eq!(
        decision, "block",
        "committed-but-unmerged .rs change (clean tree) must run the gated check, not skip it; \
         stdout={stdout:?} stderr={stderr:?}"
    );
}

#[test]
fn undeterminable_merge_base_does_not_read_as_no_changes() {
    // No `main` branch exists: the merge-base with main cannot be determined.
    let f = Fixture::new("trunk", Change::Committed);
    let (decision, stdout, stderr) = f.stop_decision();
    assert_eq!(
        decision, "block",
        "undeterminable merge-base must fail closed (check runs), not read as clean; \
         stdout={stdout:?} stderr={stderr:?}"
    );
}
