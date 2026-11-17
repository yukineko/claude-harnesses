// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! REPRO (suspected fail-open, 2026-11-18): reviewgate narrows its review set
//! to the files THIS session wrote via
//! `harness_core::attribution::attribute_own_writes`; a changed file that no
//! Edit/Write touched and no (non-read-only) Bash statement names is
//! "attributed to nobody and is EXCLUDED".
//!
//! A session that wrote files without naming them (`cargo fmt`, a glob
//! `sed -i 's/a/b/' *.rs`, a generator, `git stash pop`, a whole-tree branch
//! checkout) therefore has those files silently dropped from the review: the
//! gate returns "no-reviewable-changes" for code nobody reviewed. CLAUDE.md
//! section 3: an unattributable write is "cannot determine" and must resolve
//! to the restrictive side (keep the file in the review set).
//!
//! NOTE: the 2026-10-17 user ruling (see `session_attribution.rs` header) had
//! accepted this residual hole; it was superseded by the 2026-11-18 ruling:
//! a path-less write keeps full scope. These tests pin that ruling and the
//! CLAUDE.md section 3 reading.
//!
//! Observable: the JSONL log line (`$HOME/.reviewgate/state/log.jsonl`) holds
//! exactly the `files` the gate reviewed, plus the stdout decision, as in
//! `session_attribution.rs`.
//!
//! * `control_edit_naming_the_file_is_reviewed` (passes today)
//! * `control_read_only_session_still_excludes_foreign_change` (passes today)
//! * `unnamed_write_cargo_fmt_stays_in_review_set` /
//!   `unnamed_write_glob_sed_stays_in_review_set` - RED.
//!
//! Written by a disinterested author (CLAUDE.md 2(a)).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

const SESSION: &str = "s-unnamed-write";
const FILE: &str = "foreign.rs";

struct Outcome {
    decision: String,
    log_verdict: String,
    log_files: Vec<String>,
}

struct Fixture {
    home: PathBuf,
    repo: PathBuf,
}

fn git(dir: &Path, args: &[&str]) -> std::process::ExitStatus {
    harness_core::git_env::scrub(&mut Command::new("git"))
        .current_dir(dir)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git runs")
}

impl Fixture {
    /// A git repo with one commit and one UNTRACKED reviewable file
    /// (`FILE`) in the working tree.
    fn new() -> Fixture {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let home =
            std::env::temp_dir().join(format!("reviewgate-unnamed-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&home);
        let repo = home.join("project");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            &["init", "-q"][..],
            &["config", "user.email", "t@t.com"][..],
            &["config", "user.name", "t"][..],
        ] {
            assert!(git(&repo, args).success(), "git setup failed: {args:?}");
        }
        std::fs::write(repo.join("base.rs"), "fn base() {}\n").unwrap();
        assert!(git(&repo, &["add", "base.rs"]).success());
        assert!(git(
            &repo,
            &["-c", "commit.gpgsign=false", "commit", "-qm", "init"]
        )
        .success());
        let repo = repo.canonicalize().unwrap_or(repo);
        std::fs::create_dir_all(home.join(".reviewgate")).unwrap();
        std::fs::write(
            home.join(".reviewgate").join("config.toml"),
            "mode = \"inject\"\n",
        )
        .unwrap();
        std::fs::write(repo.join(FILE), "fn foreign() {}\n").unwrap();
        Fixture { home, repo }
    }

    /// Transcript under `$HOME/.claude/projects/<slug>/` (the layout
    /// `session_attribution.rs` documents as required).
    fn transcript(&self, uses: &[serde_json::Value]) -> PathBuf {
        let d = self.home.join(".claude/projects/-home-t-project");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join(format!("{SESSION}.jsonl"));
        let mut body = String::from(r#"{"type":"user","message":{"role":"user","content":"go"}}"#);
        body.push('\n');
        for u in uses {
            body.push_str(&u.to_string());
            body.push('\n');
        }
        std::fs::write(&p, body).unwrap();
        p
    }

    fn run(&self, tp: &Path) -> Outcome {
        let payload = serde_json::json!({
            "session_id": SESSION,
            "transcript_path": tp.to_string_lossy(),
            "cwd": self.repo.to_string_lossy(),
            "hook_event_name": "Stop",
            "stop_hook_active": false,
        })
        .to_string();
        let mut child =
            harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_reviewgate")))
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
        let decision = serde_json::from_str::<serde_json::Value>(stdout.trim())
            .ok()
            .and_then(|v| v.get("decision").and_then(|d| d.as_str()).map(String::from))
            .unwrap_or_else(|| "allow".to_string());
        let path = self.home.join(".reviewgate/state/log.jsonl");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let line = text
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .rfind(|v| v.get("session").and_then(|s| s.as_str()) == Some(SESSION))
            .unwrap_or_else(|| panic!("no log line for {SESSION} in:\n{text}"));
        let log_verdict = line
            .get("verdict")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let log_files = line
            .get("files")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        Outcome {
            decision,
            log_verdict,
            log_files,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn tool_use(name: &str, input: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "parentUuid": "p", "isSidechain": false,
        "message": {"model": "claude-opus-5", "id": "m", "type": "message", "role": "assistant",
            "content": [{"type": "tool_use", "id": "t", "name": name, "input": input}]}
    })
}

fn bash(cmd: &str) -> serde_json::Value {
    tool_use(
        "Bash",
        serde_json::json!({"command": cmd, "description": "run"}),
    )
}

fn edit(path: &Path) -> serde_json::Value {
    tool_use(
        "Edit",
        serde_json::json!({"file_path": path.to_string_lossy(), "old_string": "a", "new_string": "b"}),
    )
}

/// CONTROL (passes today): an Edit naming the file puts it in the review set.
#[test]
fn control_edit_naming_the_file_is_reviewed() {
    let f = Fixture::new();
    let t = f.transcript(&[edit(&f.repo.join(FILE))]);
    let o = f.run(&t);
    assert_eq!(o.decision, "block", "log verdict: {}", o.log_verdict);
    assert_eq!(o.log_files, vec![FILE.to_string()]);
}

/// CONTROL (passes today, must keep passing after a fix): a read-only session
/// wrote nothing, so a foreign change is excluded from the review set.
#[test]
fn control_read_only_session_still_excludes_foreign_change() {
    let f = Fixture::new();
    let t = f.transcript(&[bash("git status")]);
    let o = f.run(&t);
    assert!(
        !o.log_files.contains(&FILE.to_string()),
        "a read-only session must not own a foreign change; reviewed: {:?} (verdict {})",
        o.log_files,
        o.log_verdict
    );
}

/// RED: `cargo fmt` rewrites files without naming any. A path-less write is
/// unattributable -> the changed file must stay in the review set.
#[test]
fn unnamed_write_cargo_fmt_stays_in_review_set() {
    let f = Fixture::new();
    let t = f.transcript(&[bash("cargo fmt")]);
    let o = f.run(&t);
    assert!(
        o.log_files.contains(&FILE.to_string()),
        "a session that ran `cargo fmt` (path-less write) must keep {FILE} in the review set; \
         decision={}, verdict={}, reviewed={:?}",
        o.decision,
        o.log_verdict,
        o.log_files
    );
}

/// Like `bash`, with the `cwd` a real transcript records on the call.
fn bash_in(cmd: &str, cwd: &Path) -> serde_json::Value {
    let mut v = bash(cmd);
    v["cwd"] = serde_json::json!(cwd.to_string_lossy());
    v
}

/// RED (needs the has_unattributable_write cd-target fix, relay from the
/// coordinator 2026-11-18): `cd <abs dir> && cargo fmt` names only a directory
/// (the cd target); the formatter still rewrites files without naming any, so
/// the changed file must stay in the review set.
#[test]
fn unnamed_write_cd_abs_cargo_fmt_stays_in_review_set() {
    let f = Fixture::new();
    let t = f.transcript(&[bash_in(
        &format!("cd {} && cargo fmt", f.repo.display()),
        &f.repo,
    )]);
    let o = f.run(&t);
    assert!(
        o.log_files.contains(&FILE.to_string()),
        "`cd <abs> && cargo fmt` is a path-less write; {FILE} must stay in the review set; \
         decision={}, verdict={}, reviewed={:?}",
        o.decision,
        o.log_verdict,
        o.log_files
    );
}

/// RED (same fix): `cd <relative dir> && cargo fmt`, resolved against the call's cwd.
#[test]
fn unnamed_write_cd_relative_cargo_fmt_stays_in_review_set() {
    let f = Fixture::new();
    std::fs::create_dir_all(f.repo.join("sub")).unwrap();
    let t = f.transcript(&[bash_in("cd sub && cargo fmt", &f.repo)]);
    let o = f.run(&t);
    assert!(
        o.log_files.contains(&FILE.to_string()),
        "`cd sub && cargo fmt` is a path-less write; {FILE} must stay in the review set; \
         decision={}, verdict={}, reviewed={:?}",
        o.decision,
        o.log_verdict,
        o.log_files
    );
}

/// RED: a glob `sed -i` rewrites files without naming their paths.
#[test]
fn unnamed_write_glob_sed_stays_in_review_set() {
    let f = Fixture::new();
    let t = f.transcript(&[bash("sed -i 's/a/b/' *.rs")]);
    let o = f.run(&t);
    assert!(
        o.log_files.contains(&FILE.to_string()),
        "a session that ran a glob `sed -i` must keep {FILE} in the review set; \
         decision={}, verdict={}, reviewed={:?}",
        o.decision,
        o.log_verdict,
        o.log_files
    );
}
