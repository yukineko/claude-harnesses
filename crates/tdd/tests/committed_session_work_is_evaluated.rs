#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Spec: implementation changes the session COMMITTED during the session (in
//! any root) are evaluated like uncommitted ones. Observed defect: the session
//! wrote impl + test in a linked worktree, committed, and stopped with the hook
//! cwd in the primary tree; tdd scanned only uncommitted diffs, evaluated
//! nothing, and blocked ("an empty scan is not a pass") a test-first session.
//!
//! Approved design ("Hybrid"): per root, besides the uncommitted diff, the
//! gate evaluates the added lines of non-merge commits reachable from HEAD
//! whose COMMITTER date is >= the session start, where session start = the
//! EARLIEST `"timestamp"` on any transcript record. Those lines are narrowed by
//! the same per-file session attribution as uncommitted files. No timestamp in
//! the transcript => session start undetermined => committed work is NOT
//! credited (fail closed).
//!
//! All dates are pinned (`GIT_COMMITTER_DATE` / `GIT_AUTHOR_DATE`, UTC `Z`) so
//! the verdicts do not depend on the wall clock or the local timezone:
//!
//! * repo seed commit ............ 2026-01-01T00:00:00Z
//! * pre-session commits ......... 2026-05-01T00:00:00Z
//! * session start (earliest ts) . 2026-06-01T05:00:00Z
//! * session commits ............. 2026-06-01T05:20:00Z
//! * last transcript record ...... 2026-06-01T05:30:00Z (after the commit, so
//!   an implementation that used the LATEST timestamp would miss the commit)
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
const OLD_IMPL: &str =
    "pub fn g() -> i32 {\n    let a = 3;\n    let b = a * 2;\n    println!(\"{}\", b);\n    b\n}\n";
const TEST: &str = "#[test]\nfn f_works() {\n    assert_eq!(2, 2);\n}\n";

const SEED_DATE: &str = "2026-01-01T00:00:00Z";
const PRE_SESSION: &str = "2026-05-01T00:00:00Z";
const SESSION_START: &str = "2026-06-01T05:00:00.000Z";
const IN_SESSION_WRITE: &str = "2026-06-01T05:10:00.000Z";
const IN_SESSION_COMMIT: &str = "2026-06-01T05:20:00Z";
const SESSION_LAST: &str = "2026-06-01T05:30:00.000Z";

/// Whether the session works in a linked worktree (hook cwd = primary) or
/// directly in the hook cwd's checkout (single root).
#[derive(Clone, Copy, PartialEq)]
enum Layout {
    LinkedWorktree,
    SingleRoot,
}

struct Fixture {
    home: PathBuf,
    primary: PathBuf,
    /// Where the session writes and commits: the linked worktree, or `primary`
    /// itself for `Layout::SingleRoot`.
    work: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    git_env(dir, args, &[]);
}

fn git_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) {
    let mut c = Command::new("git");
    c.current_dir(dir).args(args);
    for (k, v) in env {
        c.env(k, v);
    }
    let st = c
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git runs");
    assert!(st.success(), "git {args:?} failed in {}", dir.display());
}

/// `git add -A && git commit` in `dir` with pinned author and committer dates.
fn commit_dated(dir: &Path, msg: &str, author_date: &str, committer_date: &str) {
    git(dir, &["add", "-A"]);
    git_env(
        dir,
        &["commit", "-qm", msg],
        &[
            ("GIT_AUTHOR_DATE", author_date),
            ("GIT_COMMITTER_DATE", committer_date),
        ],
    );
}

fn commit_at(dir: &Path, msg: &str, date: &str) {
    commit_dated(dir, msg, date, date);
}

/// One transcript record per line. `ts = None` omits the `timestamp` field.
fn user_record(ts: Option<&str>) -> serde_json::Value {
    let mut v = serde_json::json!({
        "type": "user", "uuid": "u0", "sessionId": "s-committed",
        "message": {"role": "user", "content": "go"}
    });
    if let Some(ts) = ts {
        v["timestamp"] = serde_json::json!(ts);
    }
    v
}

fn write_record(path: &Path, ts: Option<&str>) -> serde_json::Value {
    let mut v = serde_json::json!({
        "type": "assistant", "parentUuid": "u0", "isSidechain": false,
        "sessionId": "s-committed",
        "message": {"model": "claude-opus-5", "id": "m1", "type": "message",
            "role": "assistant",
            "content": [{"type": "tool_use", "id": "t1", "name": "Write",
                "input": {"file_path": path.to_string_lossy(), "content": "x"}}]}
    });
    if let Some(ts) = ts {
        v["timestamp"] = serde_json::json!(ts);
    }
    v
}

fn text_record(ts: Option<&str>) -> serde_json::Value {
    let mut v = serde_json::json!({
        "type": "assistant", "parentUuid": "m1", "isSidechain": false,
        "sessionId": "s-committed",
        "message": {"model": "claude-opus-5", "id": "m2", "type": "message",
            "role": "assistant", "content": [{"type": "text", "text": "done"}]}
    });
    if let Some(ts) = ts {
        v["timestamp"] = serde_json::json!(ts);
    }
    v
}

impl Fixture {
    fn new(layout: Layout) -> Fixture {
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
        commit_at(&primary, "init", SEED_DATE);
        let work = match layout {
            Layout::LinkedWorktree => {
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
                wt.canonicalize().unwrap()
            }
            Layout::SingleRoot => primary.canonicalize().unwrap(),
        };
        let primary = primary.canonicalize().unwrap();
        std::fs::create_dir_all(home.join(".harness")).unwrap();
        std::fs::write(
            home.join(".harness/trust.toml"),
            format!(
                "trusted = [\"{}\", \"{}\"]\n",
                primary.display(),
                work.display()
            ),
        )
        .unwrap();
        Fixture {
            home,
            primary,
            work,
        }
    }

    /// Write `rel` in the work tree (creating dirs); returns its absolute path.
    fn put(&self, rel: &str, body: &str) -> PathBuf {
        let p = self.work.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        p
    }

    /// Commit everything in the work tree, dated inside the session.
    fn commit_in_session(&self) {
        commit_at(&self.work, "session work", IN_SESSION_COMMIT);
    }

    /// Commit everything in the work tree, dated BEFORE the session start.
    fn commit_pre_session(&self) {
        commit_at(&self.work, "earlier work", PRE_SESSION);
    }

    /// Realistic transcript: an opening record WITHOUT a timestamp (as some
    /// leading meta records are), the user prompt at `SESSION_START` (the
    /// earliest timestamp), one Write record per path, and a closing text
    /// record at `SESSION_LAST` (after the in-session commit).
    fn transcript(&self, written: &[&Path]) -> PathBuf {
        let mut recs = vec![
            serde_json::json!({"type": "summary", "summary": "s", "leafUuid": "x"}),
            user_record(Some(SESSION_START)),
        ];
        for p in written {
            recs.push(write_record(p, Some(IN_SESSION_WRITE)));
        }
        recs.push(text_record(Some(SESSION_LAST)));
        self.write_transcript(&recs)
    }

    /// Same records, but no record carries a `timestamp` field.
    fn transcript_without_timestamps(&self, written: &[&Path]) -> PathBuf {
        let mut recs = vec![user_record(None)];
        for p in written {
            recs.push(write_record(p, None));
        }
        recs.push(text_record(None));
        self.write_transcript(&recs)
    }

    fn write_transcript(&self, recs: &[serde_json::Value]) -> PathBuf {
        let d = self.home.join(".claude/projects/-slug");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("sess.jsonl");
        let mut body = String::new();
        for r in recs {
            body.push_str(&r.to_string());
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

// ---------------------------------------------------------------------------
// Linked worktree (hook cwd = primary, session committed in the worktree)
// ---------------------------------------------------------------------------

/// REPRO: impl + test written and committed in the worktree during the
/// session, tree clean, hook cwd = primary. The committed work is
/// test-accompanied and must be accepted. Also pins "session start = EARLIEST
/// timestamp": the last record (05:30) is after the commit (05:20).
#[test]
fn committed_impl_with_test_is_accepted() {
    let f = Fixture::new(Layout::LinkedWorktree);
    let imp = f.put("src/lib.rs", IMPL);
    let tst = f.put("tests/it.rs", TEST);
    f.commit_in_session();
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
    let f = Fixture::new(Layout::LinkedWorktree);
    let imp = f.put("src/lib.rs", IMPL);
    f.commit_in_session();
    let tp = f.transcript(&[&imp]);
    let (stdout, stderr) = f.stop(&tp);
    assert!(
        blocks(&stdout),
        "committed untested impl must block; stdout={stdout:?} stderr={stderr:?}"
    );
}

/// (a) Test evidence committed BEFORE the session start is not this session's
/// evidence. A pre-session commit adds tests/it.rs (#[test]); an in-session
/// commit adds untested impl. The transcript also records a Write of the test
/// file, so attribution alone does not exclude it — only the date window does.
/// Crediting the pre-session commit would allow; the correct verdict blocks.
#[test]
fn pre_session_test_commit_does_not_count_as_evidence() {
    let f = Fixture::new(Layout::LinkedWorktree);
    let tst = f.put("tests/it.rs", TEST);
    f.commit_pre_session();
    let imp = f.put("src/lib.rs", IMPL);
    f.commit_in_session();
    let tp = f.transcript(&[&tst, &imp]);
    let (stdout, stderr) = f.stop(&tp);
    assert!(
        blocks(&stdout),
        "a test committed before the session started must not vouch for the session's \
         impl; stdout={stdout:?} stderr={stderr:?}"
    );
}

/// (c) No timestamp on any transcript record: the session start is
/// undetermined, so committed work cannot be scoped to the session and is not
/// credited. The session wrote + committed impl + test in the worktree; the
/// gate must NOT allow on the basis of those commits.
#[test]
fn no_transcript_timestamps_committed_work_is_not_credited() {
    let f = Fixture::new(Layout::LinkedWorktree);
    let imp = f.put("src/lib.rs", IMPL);
    let tst = f.put("tests/it.rs", TEST);
    f.commit_in_session();
    let tp = f.transcript_without_timestamps(&[&imp, &tst]);
    let (stdout, stderr) = f.stop(&tp);
    assert!(
        blocks(&stdout),
        "without a session start, committed work cannot be attributed to the session and \
         must not produce an allow; stdout={stdout:?} stderr={stderr:?}"
    );
}

// ---------------------------------------------------------------------------
// Single root (hook cwd == the checkout the session committed in)
// ---------------------------------------------------------------------------

/// (d) Single root: untested impl committed during the session must block.
/// (Today the clean tree in the sole root is the long-standing allow.)
#[test]
fn single_root_committed_impl_without_test_blocks() {
    let f = Fixture::new(Layout::SingleRoot);
    let imp = f.put("src/lib.rs", IMPL);
    f.commit_in_session();
    let tp = f.transcript(&[&imp]);
    let (stdout, stderr) = f.stop(&tp);
    assert!(
        blocks(&stdout),
        "single root: committed untested impl must block; stdout={stdout:?} stderr={stderr:?}"
    );
}

/// (d) Single root: impl + test committed during the session is accepted.
/// NOTE: this already passes on the pre-fix code (clean sole root allows); it
/// is the allow-side control for `single_root_committed_impl_without_test_blocks`.
#[test]
fn single_root_committed_impl_with_test_is_accepted() {
    let f = Fixture::new(Layout::SingleRoot);
    let imp = f.put("src/lib.rs", IMPL);
    let tst = f.put("tests/it.rs", TEST);
    f.commit_in_session();
    let tp = f.transcript(&[&imp, &tst]);
    let (stdout, stderr) = f.stop(&tp);
    assert!(
        !blocks(&stdout),
        "single root: committed impl+test must be accepted; stdout={stdout:?} stderr={stderr:?}"
    );
}

/// (b) Pre-session committed impl is not attributed to the session. Untested
/// impl (src/old.rs) was committed BEFORE the session start; the transcript
/// records the session writing src/old.rs (e.g. it rewrote it with identical
/// content, leaving no diff). The session changed nothing else. Crediting the
/// pre-session commit would block (attributed untested impl); the correct
/// verdict is the single-root allow.
#[test]
fn pre_session_untested_impl_is_not_attributed_to_session() {
    let f = Fixture::new(Layout::SingleRoot);
    let old = f.put("src/old.rs", OLD_IMPL);
    f.commit_pre_session();
    let tp = f.transcript(&[&old]);
    let (stdout, stderr) = f.stop(&tp);
    assert!(
        !blocks(&stdout),
        "impl committed before the session started must not be judged as this session's; \
         stdout={stdout:?} stderr={stderr:?}"
    );
}

/// The window is on the COMMITTER date, not the author date: a commit authored
/// before the session but committed during it (rebase / cherry-pick / amend in
/// the session) is session work. Untested impl in it must block. A gate keyed
/// on the author date would exclude it and fall through to the single-root
/// allow.
#[test]
fn window_uses_committer_date_not_author_date() {
    let f = Fixture::new(Layout::SingleRoot);
    let imp = f.put("src/lib.rs", IMPL);
    commit_dated(&f.work, "rebased", PRE_SESSION, IN_SESSION_COMMIT);
    let tp = f.transcript(&[&imp]);
    let (stdout, stderr) = f.stop(&tp);
    assert!(
        blocks(&stdout),
        "a commit committed during the session (author date earlier) is session work; \
         stdout={stdout:?} stderr={stderr:?}"
    );
}
