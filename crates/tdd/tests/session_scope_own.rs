// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! User ruling 2026-10-17: the `tdd` Stop gate judges only the uncommitted
//! changes THIS session wrote, not every uncommitted file in the checkout.
//!
//! Attribution source is the Stop payload's `transcript_path`. A changed file is
//! this session's if an Edit/Write/MultiEdit/NotebookEdit tool_use (in the
//! transcript or its `<stem>/subagents/*.jsonl` sidechains) names it, or a Bash
//! tool_use `command` mentions its absolute or repo-relative path. Files not
//! attributed that way are EXCLUDED. An unreadable/missing/empty transcript is
//! undetermined: the whole checkout is judged and the message says so.
//!
//! Pinned only at the hook boundary (block/allow in stdout JSON). Residual hole
//! this contract accepts: writes by commands that never name the path
//! (`cargo fmt`, globs, scripts) are not attributed.
//!
//! Written by a disinterested author (CLAUDE.md section 2(a)).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

const IMPL_BODY: &str = "fn f() {\n    let x = 1;\n    println!(\"{}\", x);\n}\n";

struct Fixture {
    home: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!("tdd-scope-own-{}-{n}", std::process::id()));
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
        std::fs::write(repo.join("seed.rs"), "fn seed() {}\n").unwrap();
        assert!(git(&repo, &["add", "seed.rs"]));
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
        std::fs::write(p, IMPL_BODY).unwrap();
    }

    fn abs(&self, name: &str) -> String {
        self.repo.join(name).to_string_lossy().into_owned()
    }

    fn slug_dir(&self) -> PathBuf {
        let d = self.home.join(".claude/projects/-home-t-project");
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Write `<slug>/<session>.jsonl` from `(tool, input)` pairs.
    fn transcript(&self, session: &str, uses: &[(&str, serde_json::Value)]) -> PathBuf {
        let p = self.slug_dir().join(format!("{session}.jsonl"));
        std::fs::write(&p, lines(uses)).unwrap();
        p
    }

    /// Write a subagent sidechain transcript for `session`.
    fn sidechain(&self, session: &str, uses: &[(&str, serde_json::Value)]) {
        let d = self.slug_dir().join(session).join("subagents");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("agent-1.jsonl"), lines(uses)).unwrap();
    }

    /// Run the real `tdd gate` Stop hook. Returns `(blocked, stdout, stderr)`.
    fn stop(&self, session: &str, transcript_path: &str) -> (bool, String, String) {
        let payload = serde_json::json!({
            "session_id": session,
            "transcript_path": transcript_path,
            "cwd": self.repo.to_string_lossy(),
            "hook_event_name": "Stop",
            "stop_hook_active": false,
        })
        .to_string();
        let mut child = Command::new(env!("CARGO_BIN_EXE_tdd"))
            .arg("gate")
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_SESSION_ID", session)
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
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let blocked = serde_json::from_str::<serde_json::Value>(stdout.trim())
            .ok()
            .and_then(|v| v.get("decision").and_then(|d| d.as_str()).map(String::from))
            .as_deref()
            == Some("block");
        (
            blocked,
            stdout,
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
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

fn lines(uses: &[(&str, serde_json::Value)]) -> String {
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

fn file_op(tool: &str, path: &str) -> (&'static str, serde_json::Value) {
    let name: &'static str = match tool {
        "Read" => "Read",
        "Edit" => "Edit",
        _ => "Write",
    };
    (
        name,
        serde_json::json!({"file_path": path, "old_string": "a", "new_string": "b"}),
    )
}

fn bash(cmd: &str) -> (&'static str, serde_json::Value) {
    ("Bash", serde_json::json!({ "command": cmd }))
}

fn says_undetermined(s: &str) -> bool {
    let l = s.to_lowercase();
    l.contains("undetermined") || s.contains("判定不能") || l.contains("not verified")
}

/// CONTROL: the whole checkout (no transcript) blocks on a foreign-looking file,
/// proving the gate really runs in this fixture (trust/config) — otherwise every
/// "must not block" below would be vacuous.
#[test]
fn control_whole_checkout_blocks_without_a_transcript() {
    let f = Fixture::new();
    f.dirty("foreign_impl.rs");
    let (blocked, out, err) = f.stop("s-control", "");
    assert!(
        blocked,
        "apparatus: tdd must block on an unattributable impl change, else this file proves \
         nothing. stdout={out:?} stderr={err:?}"
    );
}

/// THE BUG. Proves: a file this session only Read, never wrote, does not block
/// the session. Does NOT prove: that Bash-written files are attributed (cases below).
#[test]
fn foreign_only_does_not_block() {
    let f = Fixture::new();
    f.dirty("foreign_impl.rs");
    let tp = f.transcript(
        "this-session",
        &[
            file_op("Read", &f.abs("foreign_impl.rs")),
            file_op("Edit", &f.abs("seed.rs")),
        ],
    );
    let (blocked, out, err) = f.stop("s-foreign", tp.to_str().unwrap());
    assert!(
        !blocked,
        "tdd blocked on an uncommitted impl file this session never wrote (transcript read \
         OK, only a Read of it). Proves the scope narrowing only for an Edit-less, \
         Bash-less footprint. stdout={out:?} stderr={err:?}"
    );
}

/// Proves: own Edit (absolute path) keeps the gate blocking even when a foreign file
/// is also dirty. Does NOT prove the foreign file is excluded from the verdict
/// (tdd's hook output is only block/allow at this boundary).
#[test]
fn own_edit_still_blocks() {
    let f = Fixture::new();
    f.dirty("foreign_impl.rs");
    f.dirty("own_impl.rs");
    let tp = f.transcript("this-session", &[file_op("Edit", &f.abs("own_impl.rs"))]);
    let (blocked, out, err) = f.stop("s-own", tp.to_str().unwrap());
    assert!(
        blocked,
        "own Edit of an impl file with no test must block. stdout={out:?} stderr={err:?}"
    );
}

/// Anti-vacuity partner of `foreign_only_does_not_block`: the SAME foreign file
/// plus an own Write of a test-less file blocks, but the reason must not be
/// driven by the foreign file alone. Only observable weakly: we assert the reason
/// does not list the foreign file as changed implementation.
#[test]
fn own_edit_reason_does_not_list_the_foreign_file() {
    let f = Fixture::new();
    f.dirty("foreign_impl.rs");
    f.dirty("own_impl.rs");
    let tp = f.transcript("this-session", &[file_op("Write", &f.abs("own_impl.rs"))]);
    let (blocked, out, _err) = f.stop("s-own-reason", tp.to_str().unwrap());
    assert!(blocked, "own change must block; stdout={out:?}");
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("stdout is JSON");
    let reason = v["reason"].as_str().unwrap_or("");
    let changed_line = reason
        .lines()
        .find(|l| l.trim_start().starts_with("implementation changed:"))
        .unwrap_or_else(|| panic!("no `implementation changed:` line in reason: {reason:?}"));
    assert!(
        changed_line.contains("own_impl.rs") && !changed_line.contains("foreign_impl.rs"),
        "the `implementation changed:` line must list the own file and must not present \
         the foreign file as this session's change (naming it elsewhere, as excluded / \
         not attributed, is allowed by the ruling). line={changed_line:?}"
    );
    assert!(
        reason.contains("4 new implementation line(s)"),
        "the added-line count must cover only the own file (4), not own+foreign (8). \
         reason={reason:?}"
    );
}

/// Bash tool_use naming the ABSOLUTE path counts as own.
#[test]
fn own_bash_absolute_path_still_blocks() {
    let f = Fixture::new();
    f.dirty("foreign_impl.rs");
    f.dirty("bash_abs.rs");
    let cmd = format!("cat > {} <<'EOF'\nfn f() {{}}\nEOF", f.abs("bash_abs.rs"));
    let tp = f.transcript("this-session", &[bash(&cmd)]);
    let (blocked, out, err) = f.stop("s-bash-abs", tp.to_str().unwrap());
    assert!(
        blocked,
        "a file written only via a Bash command mentioning its absolute path is this \
         session's and must be judged. Proves nothing about foreign exclusion. \
         stdout={out:?} stderr={err:?}"
    );
}

/// Bash tool_use naming the REPO-RELATIVE path counts as own.
#[test]
fn own_bash_relative_path_still_blocks() {
    let f = Fixture::new();
    f.dirty("foreign_impl.rs");
    f.dirty("src/x.rs");
    let tp = f.transcript("this-session", &[bash("sed -i '' 's/a/b/' src/x.rs")]);
    let (blocked, out, err) = f.stop("s-bash-rel", tp.to_str().unwrap());
    assert!(
        blocked,
        "a file mentioned in a Bash command by repo-relative path is this session's. \
         stdout={out:?} stderr={err:?}"
    );
}

/// Bash mention is the ONLY thing attributing the file: with a Bash command that
/// mentions an unrelated path, the foreign file stays excluded (not blocked).
#[test]
fn bash_mentioning_an_unrelated_path_does_not_attribute_the_foreign_file() {
    let f = Fixture::new();
    f.dirty("foreign_impl.rs");
    let tp = f.transcript("this-session", &[bash("ls -la seed.rs && git status")]);
    let (blocked, out, err) = f.stop("s-bash-unrelated", tp.to_str().unwrap());
    assert!(
        !blocked,
        "a Bash command that never names foreign_impl.rs must not attribute it. \
         stdout={out:?} stderr={err:?}"
    );
}

/// Missing transcript file => undetermined => whole checkout judged + disclosed.
#[test]
fn unreadable_transcript_judges_whole_checkout_and_says_so() {
    let f = Fixture::new();
    f.dirty("foreign_impl.rs");
    let missing = f.home.join("no-such-transcript.jsonl");
    assert!(!missing.exists());
    let (blocked, out, err) = f.stop("s-unreadable", missing.to_str().unwrap());
    assert!(
        blocked,
        "unreadable transcript = attribution undetermined: must judge the whole checkout \
         (fail-closed). stdout={out:?} stderr={err:?}"
    );
    assert!(
        says_undetermined(&out) || says_undetermined(&err),
        "the message must say attribution was undetermined / not verified. NOTE: today's \
         block text always says 'NOT verified', so this passes vacuously until the \
         message is made conditional. stdout={out:?} stderr={err:?}"
    );
}

/// Empty transcript_path => undetermined, same as above.
#[test]
fn empty_transcript_path_judges_whole_checkout_and_says_so() {
    let f = Fixture::new();
    f.dirty("foreign_impl.rs");
    let (blocked, out, err) = f.stop("s-empty", "");
    assert!(
        blocked,
        "empty transcript_path = undetermined: must judge the whole checkout. \
         stdout={out:?} stderr={err:?}"
    );
    assert!(
        says_undetermined(&out) || says_undetermined(&err),
        "message must say attribution was undetermined / not verified (vacuous today, see \
         the unreadable_transcript test). stdout={out:?} stderr={err:?}"
    );
}

/// A subagent sidechain Edit counts as own.
#[test]
fn subagent_sidechain_edit_counts_as_own() {
    let f = Fixture::new();
    f.dirty("foreign_impl.rs");
    f.dirty("sub_impl.rs");
    // Parent transcript touches nothing relevant; only the sidechain wrote sub_impl.rs.
    let tp = f.transcript("this-session", &[file_op("Read", &f.abs("seed.rs"))]);
    f.sidechain("this-session", &[file_op("Edit", &f.abs("sub_impl.rs"))]);
    let (blocked, out, err) = f.stop("s-sidechain", tp.to_str().unwrap());
    assert!(
        blocked,
        "an Edit recorded only in <stem>/subagents/*.jsonl is this session's own work. \
         stdout={out:?} stderr={err:?}"
    );
}

/// Anti-vacuity for the sidechain case: WITHOUT the sidechain the same layout
/// (foreign file only) must not block — so the block above is caused by the
/// sidechain attribution, not by whole-checkout judging.
#[test]
fn without_the_sidechain_edit_the_same_layout_does_not_block() {
    let f = Fixture::new();
    f.dirty("foreign_impl.rs");
    f.dirty("sub_impl.rs");
    let tp = f.transcript("this-session", &[file_op("Read", &f.abs("seed.rs"))]);
    let (blocked, out, err) = f.stop("s-nosidechain", tp.to_str().unwrap());
    assert!(
        !blocked,
        "with no Edit/Write/Bash naming either dirty file, nothing is attributed to this \
         session and the gate must not block on others' files. stdout={out:?} stderr={err:?}"
    );
}
