// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! tdd judges ONLY the checkout the Stop hook runs in (the payload `cwd`) —
//! user ruling 2026-11-08: gates judge own worktree only ("donegateそのものが
//! 他のworktreeを見たり検証したりテストしたりすることは禁止とする。他のgateでも
//! 禁止とする"). Nothing replaces the dropped cross-worktree judging.
//!
//! History: this file began (2026-10-22) as the RED reproduction for the
//! opposite contract — roots = hook cwd + git toplevel of every file the
//! session transcript records as written, evaluated per root; "wrote files but
//! nothing evaluated" and "a written root cannot be resolved" BLOCKED. That
//! contract is withdrawn (the `session_roots` resolution and the >1-root
//! "wrote but nothing evaluated" rule are gone from the Stop hook). Tests that
//! pinned it are inverted: another worktree the session wrote into — dirty,
//! unreadable, or with a broken git probe — does NOT affect the verdict, while
//! the cwd checkout is still judged (`control_*`, `red_cwd_root_*`).
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
        // Real transcripts carry record timestamps; without one the session
        // start is undetermined and tdd BLOCKS (CLAUDE.md §3), which these
        // scenarios do not test. Placed after the fixture's setup commits.
        let mut body = String::from(
            r#"{"type":"user","timestamp":"2099-01-01T00:00:00.000Z","message":{"role":"user","content":"go"}}"#,
        );
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
        let mut child = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_tdd")))
            .arg("gate")
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_SESSION_ID", "s-wtroots")
            .env_remove("TDD_DISABLE")
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

impl LockedDir {
    /// Locks a directory that already exists (e.g. a linked worktree).
    fn new_existing(dir: &Path) -> LockedDir {
        LockedDir::new(dir)
    }
}

impl Drop for LockedDir {
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

/// INVERTED (user ruling 2026-11-08: gates judge own worktree only). Was
/// `worktree_impl_file_is_evaluated_when_cwd_is_the_primary_tree`, which demanded
/// the linked worktree's untested impl file be evaluated from a primary-tree
/// cwd. Now the other worktree is not evaluated: cwd = clean primary => allow,
/// and no block names the worktree file.
#[test]
fn dirty_other_worktree_impl_is_not_evaluated_when_cwd_is_the_primary_tree() {
    let f = Fixture::new();
    let tp = f.transcript(&[edit(&f.wt.join("session_edit.rs"))]);
    let o = f.stop(&f.primary, &tp);
    assert_ne!(
        o.decision, "block",
        "a dirty OTHER worktree must not affect the verdict; stdout={:?} stderr={:?}",
        o.stdout, o.stderr
    );
    assert!(
        !o.reason.contains("session_edit.rs") && !o.stderr.contains("session_edit.rs"),
        "the other worktree's file must not be evaluated; reason={:?} stderr={:?}",
        o.reason,
        o.stderr
    );
}

/// NEW (user ruling 2026-11-08), (c) anti-vacuity in the same fixture: the cwd
/// (primary) holds an untested impl file the session wrote => still BLOCKS,
/// naming the cwd file and the cwd checkout; the dirty linked worktree the
/// session also wrote into is not named.
#[test]
fn red_cwd_root_still_blocks_and_other_worktree_is_not_named() {
    let f = Fixture::new();
    std::fs::write(f.primary.join("primary_impl.rs"), BODY).unwrap();
    let tp = f.transcript(&[
        edit(&f.wt.join("session_edit.rs")),
        edit(&f.primary.join("primary_impl.rs")),
    ]);
    let o = f.stop(&f.primary, &tp);
    assert_eq!(
        o.decision, "block",
        "red cwd must block. stdout={:?} stderr={:?}",
        o.stdout, o.stderr
    );
    assert!(
        o.reason.contains("primary_impl.rs"),
        "reason must name the cwd file; reason={:?}",
        o.reason
    );
    assert!(
        !o.reason.contains("session_edit.rs") && !o.reason.contains("linked-wt"),
        "reason must not mention the other worktree; reason={:?}",
        o.reason
    );
}

/// NEW (user ruling 2026-11-08), (b) "not read": the other worktree is
/// unreadable (mode 000, restored on Drop) while a red cwd is judged. The cwd
/// still blocks on its own file and the unreadable neighbour adds no
/// "undetermined" block.
#[test]
fn red_cwd_blocks_with_own_file_even_when_other_worktree_is_unreadable() {
    let f = Fixture::new();
    std::fs::write(f.primary.join("primary_impl.rs"), BODY).unwrap();
    let tp = f.transcript(&[
        edit(&f.wt.join("session_edit.rs")),
        edit(&f.primary.join("primary_impl.rs")),
    ]);
    let _unlock = LockedDir::new_existing(&f.wt);
    let o = f.stop(&f.primary, &tp);
    assert_eq!(
        o.decision, "block",
        "red cwd must block. stdout={:?} stderr={:?}",
        o.stdout, o.stderr
    );
    assert!(
        o.reason.contains("primary_impl.rs") && !o.reason.contains("cannot be determined"),
        "reason must be the cwd's own missing-test block, not an undetermined-roots one; reason={:?}",
        o.reason
    );
}

/// NEW (user ruling 2026-11-08), (b): the linked worktree the session wrote
/// into is unreadable and the cwd is clean => allow (no undetermined block).
#[test]
fn unreadable_other_worktree_does_not_block() {
    let f = Fixture::new();
    let tp = f.transcript(&[edit(&f.wt.join("session_edit.rs"))]);
    let _unlock = LockedDir::new_existing(&f.wt);
    let o = f.stop(&f.primary, &tp);
    assert_ne!(
        o.decision, "block",
        "an unreadable OTHER worktree must not block. stdout={:?} stderr={:?}",
        o.stdout, o.stderr
    );
}

/// INVERTED (user ruling 2026-11-08: gates judge own worktree only). Was
/// `wrote_files_but_nothing_evaluated_blocks_naming_roots` (the >1-roots
/// "wrote files but no implementation change was evaluated" rule, removed). A
/// recorded Edit of a clean file in another worktree, cwd = clean primary, is
/// now a plain allow.
#[test]
fn wrote_files_in_clean_other_worktree_allows() {
    let f = Fixture::new();
    let tp = f.transcript(&[edit(&f.wt.join("seed.rs"))]);
    let o = f.stop(&f.primary, &tp);
    assert_ne!(
        o.decision, "block",
        "the other worktree must not produce a block. stdout={:?} stderr={:?}",
        o.stdout, o.stderr
    );
}

/// INVERTED (user ruling 2026-11-08: gates judge own worktree only). Was
/// `written_root_gone_blocks_naming_the_root`: a written path under a dir git
/// cannot enter (mode 000) was an undetermined root and blocked ("roots cannot
/// be determined"). That block is gone; the location is never probed.
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
        !o.reason.contains("gone-root-4711") && !o.reason.contains("cannot be determined"),
        "reason must not report undetermined roots; reason={:?}",
        o.reason
    );
}

/// INVERTED (user ruling 2026-11-08: gates judge own worktree only). Was
/// `written_root_corrupt_gitfile_blocks_naming_the_root` (re-anchored for
/// backlog 775e5bc8): a written file whose repo has a corrupt `.git` gitfile
/// used to leave the root undetermined and block. Its git probe is no longer
/// run, so it cannot block.
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
        !o.reason.contains("plain-dir-4712") && !o.reason.contains("cannot be determined"),
        "reason must not report the other location; reason={:?}",
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

/// user ruling 2026-10-27: the session wrote into a linked worktree that was
/// removed before any resolve recorded it (isolated HOME, so no store record).
/// Its directory is gone under a base that is positively outside every repo:
/// the path is excluded, so the stop must NOT block with undetermined roots.
/// (Stale note, user ruling 2026-11-08: gates judge own worktree only — the
/// mode-000 "control" that used to block is now also an allow, see
/// `unprobeable_written_path_does_not_block`; this property holds trivially.)
#[test]
fn removed_unrecorded_worktree_under_non_repo_base_does_not_block() {
    let f = Fixture::new();
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .arg("-C")
        .arg(&f.home)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(128), "precondition: {out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("not a git repository (or any of the parent directories)"),
        "precondition: home must be outside every repo: {out:?}"
    );
    let rm = f.home.join("rm-wt");
    git(
        &f.primary,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "rm-branch",
            rm.to_str().unwrap(),
        ],
    );
    let written = rm.join("src").join("a.rs");
    std::fs::create_dir_all(written.parent().unwrap()).unwrap();
    std::fs::write(&written, "fn f() {}\n").unwrap();
    let tp = f.transcript(&[edit(&written)]);
    git(
        &f.primary,
        &["worktree", "remove", "--force", rm.to_str().unwrap()],
    );
    assert!(!rm.exists(), "precondition: the worktree dir must be gone");
    assert!(
        !f.home.join(".harness/worktree-origins.jsonl").exists(),
        "precondition: no worktree-origin record"
    );
    let o = f.stop(&f.primary, &tp);
    assert_ne!(
        o.decision, "block",
        "a write into a removed worktree under a non-repo base must not block; \
         stdout={:?} stderr={:?}",
        o.stdout, o.stderr
    );
    assert!(
        !o.reason.contains("cannot be determined") && !o.stdout.contains("cannot be determined"),
        "must not report undetermined roots; reason={:?}",
        o.reason
    );
}
