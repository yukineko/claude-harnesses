// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! reviewgate judges ONLY the checkout the Stop hook runs in (the payload
//! `cwd`) — user ruling 2026-11-08: gates judge own worktree only
//! ("donegateそのものが他のworktreeを見たり検証したりテストしたりすることは禁止
//! とする。他のgateでも禁止とする"). Nothing replaces the dropped cross-worktree
//! judging.
//!
//! History: this file began (2026-10-22) as the RED reproduction for the
//! opposite contract — roots = hook cwd ∪ git toplevel of every file the
//! session transcript records as written, a written root that cannot be
//! resolved or an empty review set BLOCKS. That contract is withdrawn: a
//! linked worktree the session wrote into is not read, reviewed or probed, so
//! it can neither add a finding nor an "undetermined" block. The tests below
//! that pinned the old contract are inverted (they now assert the other
//! worktree does NOT affect the verdict); the cwd checkout is still judged
//! (`control_*` and `red_cwd_root_still_blocks_*`, anti-vacuity).
//!
//! Fixture: `$HOME` is a tempdir with `mode = "inject"` (reviewgate's default
//! is off) and a trust list naming BOTH trees; `HARNESS_TRUST_ALL` removed.
//! `control_cwd_is_the_worktree_reviews_the_file` shows the gate does review
//! the worktree file when it is the cwd (anti-vacuity).
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
    let st = harness_core::git_env::scrub(&mut Command::new("git"))
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
    log_verdict: Option<String>,
    log_files: Vec<String>,
}

impl Fixture {
    fn new() -> Fixture {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!("rg-wtroots-{}-{n}", std::process::id()));
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
        std::fs::create_dir_all(home.join(".reviewgate")).unwrap();
        std::fs::write(home.join(".reviewgate/config.toml"), "mode = \"inject\"\n").unwrap();
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
        let mut child =
            harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_reviewgate")))
                .arg("review")
                .current_dir(cwd)
                .env("HOME", &self.home)
                .env("CLAUDE_CODE_SESSION_ID", "s-wtroots")
                .env_remove("REVIEWGATE_DISABLE")
                .env_remove("HARNESS_TRUST_ALL")
                .env_remove("HARNESS_WORKTREE_ORIGIN_DIR")
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
        let log = std::fs::read_to_string(self.home.join(".reviewgate/state/log.jsonl"))
            .unwrap_or_default();
        let line = log
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .next_back();
        let log_verdict = line
            .as_ref()
            .and_then(|l| l.get("verdict"))
            .and_then(|x| x.as_str())
            .map(String::from);
        let log_files = line
            .as_ref()
            .and_then(|l| l.get("files"))
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        Outcome {
            decision: g("decision"),
            reason: g("reason"),
            stdout,
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
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

/// Creates `dir` with mode 000 on construction and restores 0o755 on drop, so
/// teardown can delete it even when an assert panics. Only call it after the
/// transcript (which lives under the same home) is written.
struct LockedDir(PathBuf);

impl LockedDir {
    fn new(dir: &Path) -> LockedDir {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        LockedDir(dir.to_path_buf())
    }
}

impl Drop for LockedDir {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// Chmods an EXISTING dir to 000 and restores 0o755 on drop.
struct Unreadable(PathBuf);

impl Unreadable {
    fn new(dir: &Path) -> Unreadable {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        Unreadable(dir.to_path_buf())
    }
}

impl Drop for Unreadable {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
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

/// ANTI-VACUITY CONTROL: cwd = the worktree reviews the file (mode inject,
/// trusted), so the tests below cannot be an off/untrusted allow.
#[test]
fn control_cwd_is_the_worktree_reviews_the_file() {
    let f = Fixture::new();
    let tp = f.transcript(&[edit(&f.wt.join("session_edit.rs"))]);
    let o = f.stop(&f.wt, &tp);
    assert_eq!(
        o.decision, "block",
        "apparatus: {} / {}",
        o.stdout, o.stderr
    );
    assert!(
        o.log_files.iter().any(|s| s.contains("session_edit.rs")),
        "apparatus: log files={:?}",
        o.log_files
    );
}

/// INVERTED (user ruling 2026-11-08: gates judge own worktree only). Was
/// `worktree_file_is_reviewed_when_cwd_is_the_primary_tree`, which demanded the
/// linked worktree's dirty file be reviewed from a primary-tree cwd. Now the
/// worktree the session wrote into is NOT reviewed: cwd = clean primary =>
/// `no-reviewable-changes`, allow, and the worktree file is not in the review set.
#[test]
fn dirty_other_worktree_is_not_reviewed_when_cwd_is_the_primary_tree() {
    let f = Fixture::new();
    let tp = f.transcript(&[edit(&f.wt.join("session_edit.rs"))]);
    let o = f.stop(&f.primary, &tp);
    assert_ne!(
        o.decision, "block",
        "a dirty OTHER worktree must not affect the verdict; stdout={:?} stderr={:?}",
        o.stdout, o.stderr
    );
    assert_eq!(
        o.log_verdict.as_deref(),
        Some("no-reviewable-changes"),
        "the gate must have run and judged the clean cwd; stderr={:?}",
        o.stderr
    );
    assert!(
        !o.log_files.iter().any(|s| s.contains("session_edit.rs")),
        "the other worktree's file must not be in the review set; got {:?}",
        o.log_files
    );
}

/// Same fixture, with the cwd (primary) ALSO dirty and edited by the session:
/// the cwd root is still judged (red cwd => block naming ONLY the cwd file),
/// while the dirty linked worktree stays out of the review set. Anti-vacuity
/// for the inversion above.
#[test]
fn red_cwd_root_still_blocks_and_other_worktree_is_not_in_the_set() {
    let f = Fixture::new();
    std::fs::write(f.primary.join("primary_edit.rs"), BODY).unwrap();
    let tp = f.transcript(&[
        edit(&f.wt.join("session_edit.rs")),
        edit(&f.primary.join("primary_edit.rs")),
    ]);
    let o = f.stop(&f.primary, &tp);
    assert_eq!(
        o.decision, "block",
        "red cwd must block. stdout={:?} stderr={:?}",
        o.stdout, o.stderr
    );
    assert!(
        o.log_files.iter().any(|s| s.contains("primary_edit.rs")),
        "cwd file must be reviewed; got {:?}",
        o.log_files
    );
    assert!(
        !o.log_files.iter().any(|s| s.contains("session_edit.rs")),
        "other worktree's file must not be reviewed; got {:?}",
        o.log_files
    );
    assert!(
        !o.reason.contains("session_edit.rs") && !o.reason.contains("linked-wt"),
        "block reason must not mention the other worktree; reason={:?}",
        o.reason
    );
}

/// INVERTED (user ruling 2026-11-08: gates judge own worktree only). Was
/// `wrote_files_but_empty_review_set_blocks_naming_roots` (tag
/// `wrote-but-nothing-reviewed`, removed with the multi-root rule). A recorded
/// Edit in a clean linked worktree with a clean primary cwd is now simply
/// `no-reviewable-changes` => allow; nothing about the other worktree is blocked.
#[test]
fn wrote_files_in_clean_other_worktree_with_empty_review_set_allows() {
    let f = Fixture::new();
    let tp = f.transcript(&[edit(&f.wt.join("seed.rs"))]);
    let o = f.stop(&f.primary, &tp);
    assert_ne!(
        o.decision, "block",
        "the other worktree must not produce a block. stdout={:?} stderr={:?}",
        o.stdout, o.stderr
    );
    assert_eq!(
        o.log_verdict.as_deref(),
        Some("no-reviewable-changes"),
        "apparatus: the gate must have run; stderr={:?}",
        o.stderr
    );
}

/// INVERTED (user ruling 2026-11-08: gates judge own worktree only). Was
/// `written_root_gone_blocks_naming_the_root` (tag `session-roots-undetermined`,
/// removed). A written path under a directory git cannot enter (mode 000) was an
/// undetermined root and blocked; the dir is not a root any more and is never
/// probed, so the stop allows.
#[test]
fn unprobeable_written_path_does_not_block() {
    let f = Fixture::new();
    let locked = f.home.join("gone-root-4711");
    let gone = locked.join("gone").join("x.rs");
    let tp = f.transcript(&[edit(&gone)]);
    let _unlock = LockedDir::new(&locked);
    let o = f.stop(&f.primary, &tp);
    assert_ne!(
        o.decision, "block",
        "an unprobeable OTHER location must not block. stdout={:?} stderr={:?}",
        o.stdout, o.stderr
    );
    assert!(
        !o.reason.contains("gone-root-4711"),
        "reason must not name the unprobed location; reason={:?}",
        o.reason
    );
    assert_eq!(
        o.log_verdict.as_deref(),
        Some("no-reviewable-changes"),
        "apparatus: the gate must have run; stderr={:?}",
        o.stderr
    );
}

/// INVERTED (user ruling 2026-11-08: gates judge own worktree only). Was
/// `written_root_corrupt_gitfile_blocks_naming_the_root` (re-anchored for
/// backlog 775e5bc8): a written file whose git root cannot be determined (a
/// corrupt `.git` gitfile) used to block naming the root. That root is no longer
/// resolved, so its git probe failing cannot create an undetermined block.
#[test]
fn written_path_with_corrupt_gitfile_does_not_block() {
    let f = Fixture::new();
    let plain = f.home.join("plain-dir-4712");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join(".git"), "gitdir: /nonexistent/harness-test\n").unwrap();
    std::fs::write(plain.join("x.rs"), BODY).unwrap();
    let tp = f.transcript(&[edit(&plain.join("x.rs"))]);
    let o = f.stop(&f.primary, &tp);
    assert_ne!(
        o.decision, "block",
        "a broken OTHER repo must not block. stdout={:?} stderr={:?}",
        o.stdout, o.stderr
    );
    assert!(
        !o.reason.contains("plain-dir-4712"),
        "reason must not name the other location; reason={:?}",
        o.reason
    );
    assert_eq!(
        o.log_verdict.as_deref(),
        Some("no-reviewable-changes"),
        "apparatus: the gate must have run; stderr={:?}",
        o.stderr
    );
}

/// NEW (user ruling 2026-11-08: gates judge own worktree only), (b) "not read":
/// the linked worktree the session wrote into is made unreadable (mode 000,
/// restored on Drop). It is never opened, so there is no undetermined block; the
/// verdict is the clean primary's `no-reviewable-changes`.
#[test]
fn unreadable_other_worktree_does_not_block() {
    let f = Fixture::new();
    let tp = f.transcript(&[edit(&f.wt.join("session_edit.rs"))]);
    let _unlock = Unreadable::new(&f.wt);
    let o = f.stop(&f.primary, &tp);
    assert_ne!(
        o.decision, "block",
        "an unreadable OTHER worktree must not block. stdout={:?} stderr={:?}",
        o.stdout, o.stderr
    );
    assert_eq!(
        o.log_verdict.as_deref(),
        Some("no-reviewable-changes"),
        "apparatus: the gate must have run and judged the clean cwd; stderr={:?}",
        o.stderr
    );
}

/// NEW, (b)+(c) in one fixture: the other worktree is unreadable AND the cwd is
/// red => still a block on the cwd file (the unreadable neighbour neither
/// downgrades nor replaces the cwd verdict).
#[test]
fn red_cwd_blocks_even_when_other_worktree_is_unreadable() {
    let f = Fixture::new();
    std::fs::write(f.primary.join("primary_edit.rs"), BODY).unwrap();
    let tp = f.transcript(&[
        edit(&f.wt.join("session_edit.rs")),
        edit(&f.primary.join("primary_edit.rs")),
    ]);
    let _unlock = Unreadable::new(&f.wt);
    let o = f.stop(&f.primary, &tp);
    assert_eq!(
        o.decision, "block",
        "red cwd must block. stdout={:?} stderr={:?}",
        o.stdout, o.stderr
    );
    assert!(
        o.log_files.iter().any(|s| s.contains("primary_edit.rs")),
        "cwd file must be reviewed; got {:?}",
        o.log_files
    );
}

/// CONTROL: no recorded writes (only a Read) still allows.
#[test]
fn control_session_with_no_writes_still_allows() {
    let f = Fixture::new();
    let tp = f.transcript(&[read(&f.wt.join("session_edit.rs"))]);
    let o = f.stop(&f.primary, &tp);
    assert_ne!(o.decision, "block", "no writes must allow; {:?}", o.reason);
    assert_eq!(
        o.log_verdict.as_deref(),
        Some("no-reviewable-changes"),
        "apparatus: the gate must have run and judged the clean primary; stderr={:?}",
        o.stderr
    );
}
