// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED reproduction: the session worked in a LINKED git worktree while the
//! Stop-hook `cwd` was the primary tree (observed 2026-10-22 00:00:07: donegate
//! logged `{"attempt":0,"checks":[],"verdict":"green"}` although the worktree
//! held uncommitted session edits). donegate uses `input.cwd_or_current()` as
//! its only root, so it judged a tree the session never touched.
//!
//! Intended contract: roots = hook cwd ∪ git toplevel of every file the
//! session transcript records as written; the gate runs per root. If the
//! session wrote files but the check set is empty, or a written root cannot be
//! resolved (path gone / not a git repo), the stop BLOCKS with a reason naming
//! the roots. A session with no recorded writes still allows.
//!
//! Fixture: `$HOME` is a tempdir; its `.harness/trust.toml` lists BOTH the
//! primary tree and the linked worktree (canonical paths); `HARNESS_TRUST_ALL`
//! is removed. So an allow cannot come from an untrusted-root degrade.
//! `control_cwd_is_the_worktree_runs_its_check` proves the worktree's check
//! really runs when it IS the cwd (anti-vacuity for the RED test).
//!
//! Written by a disinterested author (CLAUDE.md section 2(a)).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

const MARKER: &str = "ran.marker";

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
    /// `primary_toml` is committed in the primary tree; `wt_toml` overwrites
    /// `donegate.toml` (uncommitted) in the linked worktree.
    fn new(primary_toml: &str, wt_toml: &str) -> Fixture {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!("dg-wtroots-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let primary = home.join("primary");
        std::fs::create_dir_all(&primary).unwrap();
        git(&primary, &["init", "-q"]);
        git(&primary, &["config", "user.email", "t@t.com"]);
        git(&primary, &["config", "user.name", "t"]);
        std::fs::write(primary.join("seed.rs"), "fn seed() {}\n").unwrap();
        std::fs::write(primary.join("donegate.toml"), primary_toml).unwrap();
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
        // The session's uncommitted edits live in the linked worktree only.
        std::fs::write(wt.join("donegate.toml"), wt_toml).unwrap();
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

    /// Run `donegate gate` with hook `cwd` = `cwd`. Returns (decision, reason, stdout, stderr).
    fn stop(&self, cwd: &Path, transcript: &Path) -> (String, String, String, String) {
        let payload = serde_json::json!({
            "session_id": "s-wtroots",
            "transcript_path": transcript.to_string_lossy(),
            "cwd": cwd.to_string_lossy(),
            "hook_event_name": "Stop",
            "stop_hook_active": false,
        })
        .to_string();
        let mut child = Command::new(env!("CARGO_BIN_EXE_donegate"))
            .arg("gate")
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_SESSION_ID", "s-wtroots")
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

fn exit0_toml() -> String {
    "max_attempts = 50\n\n[[check]]\nname = \"primary-ok\"\ncmd = \"exit 0\"\n".to_string()
}

fn marker_fail_toml() -> String {
    format!(
        "max_attempts = 50\n\n[[check]]\nname = \"wt-check\"\ncmd = \"echo ran > {MARKER}; exit 1\"\n"
    )
}

/// ANTI-VACUITY CONTROL: with the worktree itself as cwd, its declared check
/// runs (marker written) and blocks. Proves the worktree is trusted and its
/// config honored, so the RED test below cannot be a trust-degrade allow.
#[test]
fn control_cwd_is_the_worktree_runs_its_check() {
    let f = Fixture::new(&exit0_toml(), &marker_fail_toml());
    let tp = f.transcript(&[edit(&f.wt.join("session_edit.rs"))]);
    let (decision, reason, out, err) = f.stop(&f.wt, &tp);
    assert!(
        f.wt.join(MARKER).exists(),
        "apparatus: the worktree check must run when cwd is the worktree; out={out:?} err={err:?}"
    );
    assert_eq!(decision, "block", "reason={reason:?} err={err:?}");
}

/// THE BUG. Hook cwd = primary tree, the session's Edit names a file in the
/// linked worktree: donegate must run the WORKTREE's declared check.
#[test]
fn worktree_check_runs_when_cwd_is_the_primary_tree() {
    let f = Fixture::new(&exit0_toml(), &marker_fail_toml());
    let tp = f.transcript(&[edit(&f.wt.join("session_edit.rs"))]);
    let (decision, reason, out, err) = f.stop(&f.primary, &tp);
    assert!(
        f.wt.join(MARKER).exists(),
        "donegate never ran the linked worktree's check (checks:[] / green at cwd=primary). \
         decision={decision:?} out={out:?} err={err:?}"
    );
    assert_eq!(
        decision, "block",
        "the worktree check exits 1 so the stop must block; reason={reason:?}"
    );
}

/// The session wrote files but every root's check set is empty: that is
/// "nothing was verified", not "green". Must block, naming the roots.
#[test]
fn wrote_files_but_empty_check_set_blocks_naming_roots() {
    let empty = "max_attempts = 50\n";
    let f = Fixture::new(empty, empty);
    let tp = f.transcript(&[edit(&f.wt.join("session_edit.rs"))]);
    let (decision, reason, out, err) = f.stop(&f.primary, &tp);
    assert_eq!(
        decision, "block",
        "session wrote files, check set empty: must block. out={out:?} err={err:?}"
    );
    assert!(
        reason.contains(&f.wt.display().to_string())
            && reason.contains(&f.primary.display().to_string()),
        "reason must name both roots; reason={reason:?}"
    );
}

/// A written root that is gone cannot be resolved: undetermined -> block,
/// naming the root.
#[test]
fn written_root_gone_blocks_naming_the_root() {
    let f = Fixture::new(&exit0_toml(), &exit0_toml());
    let gone = f.home.join("gone-root-4711").join("x.rs");
    let tp = f.transcript(&[edit(&gone)]);
    let (decision, reason, out, err) = f.stop(&f.primary, &tp);
    assert_eq!(
        decision, "block",
        "unresolvable written root must block. out={out:?} err={err:?}"
    );
    assert!(
        reason.contains("gone-root-4711"),
        "reason must name the unresolvable root; reason={reason:?}"
    );
}

/// A written file whose directory exists but whose git root cannot be
/// determined (a corrupt `.git` gitfile pointing nowhere): same.
///
/// Re-anchored for backlog 775e5bc8 (GitHub 1459): this test used a plain
/// non-git directory, which the user ruling now treats as POSITIVELY outside
/// every repo and excludes from the roots. A corrupt gitfile makes git say
/// "not a git repository" without "(or any of the parent directories)", so the
/// root stays undetermined and must still block.
#[test]
fn written_root_corrupt_gitfile_blocks_naming_the_root() {
    let f = Fixture::new(&exit0_toml(), &exit0_toml());
    let plain = f.home.join("plain-dir-4712");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join(".git"), "gitdir: /nonexistent/harness-test\n").unwrap();
    std::fs::write(plain.join("x.rs"), "fn x() {}\n").unwrap();
    let tp = f.transcript(&[edit(&plain.join("x.rs"))]);
    let (decision, reason, out, err) = f.stop(&f.primary, &tp);
    assert_eq!(
        decision, "block",
        "non-git written root must block. out={out:?} err={err:?}"
    );
    assert!(
        reason.contains("plain-dir-4712"),
        "reason must name the unresolvable root; reason={reason:?}"
    );
}

/// CONTROL: a transcript recording no writes (only a Read) still allows, and
/// the primary tree's own green check ran (not a vacuous allow).
#[test]
fn control_session_with_no_writes_still_allows() {
    let f = Fixture::new(
        &format!(
            "max_attempts = 50\n\n[[check]]\nname = \"primary-ok\"\ncmd = \"echo ran > {MARKER}; exit 0\"\n"
        ),
        &marker_fail_toml(),
    );
    let tp = f.transcript(&[read(&f.wt.join("session_edit.rs"))]);
    let (decision, reason, out, err) = f.stop(&f.primary, &tp);
    assert_ne!(
        decision, "block",
        "no recorded writes must allow; reason={reason:?} out={out:?}"
    );
    assert!(
        f.primary.join(MARKER).exists(),
        "apparatus: the primary tree's check must have run; err={err:?}"
    );
}
