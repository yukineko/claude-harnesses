// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! User ruling 2026-10-17: `reviewgate review` reviews only the uncommitted
//! changes THIS session wrote, not every uncommitted file in the checkout.
//!
//! Attribution source is the Stop payload's `transcript_path`. A changed file is
//! this session's if an Edit/Write/MultiEdit/NotebookEdit tool_use (in the
//! transcript or its `<stem>/subagents/*.jsonl` sidechains) names it, or a Bash
//! tool_use `command` mentions its absolute or repo-relative path. Unattributed
//! files are EXCLUDED (no peer transcript needed). Unreadable/missing/empty
//! transcript = undetermined: review everything and say so.
//!
//! Observation channels: stdout JSON (decision, reason), stderr, and the
//! `files` list in `$HOME/.reviewgate/state/log.jsonl`.
//!
//! Residual hole accepted by the ruling: writes by commands that never name the
//! path (`cargo fmt`, globs, scripts) are not attributed.
//!
//! Written by a disinterested author (CLAUDE.md section 2(a)).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

struct Outcome {
    decision: String,
    reason: String,
    stderr: String,
    log_files: Vec<String>,
}

struct Fixture {
    home: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let home =
            std::env::temp_dir().join(format!("reviewgate-scope-own-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let repo = home.join("project");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            &["init", "-q"][..],
            &["config", "user.email", "t@t.com"][..],
            &["config", "user.name", "t"][..],
        ] {
            assert!(git(&repo, args), "git setup failed: {args:?}");
        }
        std::fs::write(repo.join("base.rs"), "fn base() {}\n").unwrap();
        assert!(git(&repo, &["add", "base.rs"]));
        assert!(git(
            &repo,
            &["-c", "commit.gpgsign=false", "commit", "-qm", "init"]
        ));
        let repo = repo.canonicalize().unwrap_or(repo);
        Fixture { home, repo }
    }

    fn dirty(&self, name: &str) {
        let p = self.repo.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, "fn dirty() {}\n").unwrap();
    }

    fn abs(&self, name: &str) -> String {
        self.repo.join(name).to_string_lossy().into_owned()
    }

    fn slug_dir(&self) -> PathBuf {
        let d = self.home.join(".claude/projects/-home-t-project");
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn transcript(&self, uses: &[(&'static str, serde_json::Value)]) -> PathBuf {
        let p = self.slug_dir().join("this-session.jsonl");
        std::fs::write(&p, lines(uses)).unwrap();
        p
    }

    fn run(&self, session: &str, tp: &str) -> Outcome {
        let payload = serde_json::json!({
            "session_id": session,
            "transcript_path": tp,
            "cwd": self.repo.to_string_lossy(),
            "hook_event_name": "Stop",
            "stop_hook_active": false,
        })
        .to_string();
        let mut child = Command::new(env!("CARGO_BIN_EXE_reviewgate"))
            .arg("review")
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env_remove("REVIEWGATE_DISABLE")
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
        let (decision, reason) = match serde_json::from_str::<serde_json::Value>(stdout.trim()) {
            Ok(v) => (
                v["decision"].as_str().unwrap_or("").to_string(),
                v["reason"].as_str().unwrap_or("").to_string(),
            ),
            Err(_) => ("allow".to_string(), String::new()),
        };
        Outcome {
            decision,
            reason,
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            log_files: self.last_log_files(session),
        }
    }

    /// `files` of the latest log line for `session`; empty when no line exists
    /// (an allow that logs nothing must not panic the test).
    fn last_log_files(&self, session: &str) -> Vec<String> {
        let path = self.home.join(".reviewgate/state/log.jsonl");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return vec![];
        };
        text.lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .rfind(|v| v["session"].as_str() == Some(session))
            .and_then(|v| {
                v["files"].as_array().map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
            })
            .unwrap_or_default()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn git(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .current_dir(dir)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn lines(uses: &[(&'static str, serde_json::Value)]) -> String {
    let mut body = String::from(r#"{"type":"user","message":{"role":"user","content":"go"}}"#);
    body.push('\n');
    for (tool, input) in uses {
        body.push_str(
            &serde_json::json!({
                "parentUuid": "p",
                "isSidechain": false,
                "message": {
                    "model": "claude-opus-5", "id": "msg_1", "type": "message",
                    "role": "assistant",
                    "content": [{"type": "tool_use", "id": "toolu_1", "name": tool, "input": input}]
                }
            })
            .to_string(),
        );
        body.push('\n');
    }
    body
}

fn file_op(tool: &'static str, path: &str) -> (&'static str, serde_json::Value) {
    (
        tool,
        serde_json::json!({"file_path": path, "old_string": "a", "new_string": "b"}),
    )
}

fn bash(cmd: &str) -> (&'static str, serde_json::Value) {
    ("Bash", serde_json::json!({ "command": cmd }))
}

fn says_undetermined(o: &Outcome) -> bool {
    let all = format!("{}\n{}", o.reason, o.stderr);
    all.to_lowercase().contains("undetermined") || all.contains("判定不能")
}

/// THE BUG. Proves: a dirty file this session never wrote (only Read) is not
/// reviewed and does not block, with a successfully read transcript and NO peer
/// transcript on disk. Does NOT prove Bash-written files are attributed.
#[test]
fn foreign_only_does_not_block() {
    let f = Fixture::new();
    f.dirty("foreign.rs");
    let tp = f.transcript(&[
        file_op("Read", &f.abs("foreign.rs")),
        file_op("Edit", &f.abs("base.rs")),
    ]);
    let o = f.run("s-foreign", tp.to_str().unwrap());
    assert_eq!(
        o.decision, "allow",
        "reviewgate blocked on a file this session never wrote. reason:\n{}\nstderr:\n{}\nfiles:{:?}",
        o.reason, o.stderr, o.log_files
    );
}

/// Proves: with a foreign file and an own Edit, the reviewed set is exactly the
/// own file and the excluded file is named (reason or stderr). Does NOT prove
/// anything about Bash attribution.
#[test]
fn own_edit_reviews_only_own_file_and_names_the_excluded_one() {
    let f = Fixture::new();
    f.dirty("foreign.rs");
    f.dirty("own.rs");
    let tp = f.transcript(&[file_op("Edit", &f.abs("own.rs"))]);
    let o = f.run("s-own", tp.to_str().unwrap());
    assert_eq!(o.decision, "block", "own change must be reviewed");
    assert_eq!(
        o.log_files,
        vec!["own.rs".to_string()],
        "the reviewed set must be exactly this session's file; foreign.rs must be excluded"
    );
    assert!(
        o.reason.contains("foreign.rs") || o.stderr.contains("foreign.rs"),
        "the excluded file must be NAMED (silent drop is as bad as silent claim). \
         reason:\n{}\nstderr:\n{}",
        o.reason,
        o.stderr
    );
}

/// Bash tool_use naming the ABSOLUTE path counts as own.
#[test]
fn own_bash_absolute_path_is_reviewed() {
    let f = Fixture::new();
    f.dirty("foreign.rs");
    f.dirty("bash_abs.rs");
    let cmd = format!("cat > {} <<'EOF'\nfn f() {{}}\nEOF", f.abs("bash_abs.rs"));
    let tp = f.transcript(&[bash(&cmd)]);
    let o = f.run("s-bash-abs", tp.to_str().unwrap());
    assert_eq!(o.decision, "block");
    assert_eq!(
        o.log_files,
        vec!["bash_abs.rs".to_string()],
        "a file written via a Bash command naming its absolute path is own; foreign.rs is not"
    );
}

/// Bash tool_use naming the REPO-RELATIVE path counts as own.
#[test]
fn own_bash_relative_path_is_reviewed() {
    let f = Fixture::new();
    f.dirty("foreign.rs");
    f.dirty("src/x.rs");
    let tp = f.transcript(&[bash("sed -i '' 's/a/b/' src/x.rs")]);
    let o = f.run("s-bash-rel", tp.to_str().unwrap());
    assert_eq!(o.decision, "block");
    assert_eq!(
        o.log_files,
        vec!["src/x.rs".to_string()],
        "a file mentioned by repo-relative path in a Bash command is own; foreign.rs is not"
    );
}

/// Missing transcript file => undetermined => review everything and say so.
#[test]
fn unreadable_transcript_reviews_everything_and_says_undetermined() {
    let f = Fixture::new();
    f.dirty("foreign.rs");
    f.dirty("own.rs");
    let missing = f.home.join("no-such-transcript.jsonl");
    assert!(!missing.exists());
    let o = f.run("s-unreadable", missing.to_str().unwrap());
    assert_eq!(o.decision, "block");
    assert_eq!(
        o.log_files,
        vec!["foreign.rs".to_string(), "own.rs".to_string()],
        "unreadable transcript = undetermined: the whole checkout is reviewed"
    );
    assert!(
        says_undetermined(&o),
        "must say attribution was undetermined / 判定不能. reason:\n{}\nstderr:\n{}",
        o.reason,
        o.stderr
    );
}

/// Empty transcript_path => same as above.
#[test]
fn empty_transcript_path_reviews_everything_and_says_undetermined() {
    let f = Fixture::new();
    f.dirty("foreign.rs");
    f.dirty("own.rs");
    let o = f.run("s-empty", "");
    assert_eq!(o.decision, "block");
    assert_eq!(
        o.log_files,
        vec!["foreign.rs".to_string(), "own.rs".to_string()],
        "empty transcript_path = undetermined: the whole checkout is reviewed"
    );
    assert!(
        says_undetermined(&o),
        "must say attribution was undetermined / 判定不能. reason:\n{}\nstderr:\n{}",
        o.reason,
        o.stderr
    );
}
