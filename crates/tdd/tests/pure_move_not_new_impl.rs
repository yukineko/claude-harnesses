#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Spec: a pure MOVE of code is not new implementation. Observed defect: a fn
//! cut from `src/a.rs` and pasted into a new `src/b.rs` (plus `mod b;`) blocks
//! with "write a test first -- N new implementation line(s) added", although
//! no behaviour changed; the only escape was a manual `tdd skip`.
//!
//! Correct behaviour encoded here:
//!
//! * An added impl line whose text, after trimming whitespace and removing a
//!   leading visibility qualifier (`pub `, `pub(crate) `, `pub(super) `,
//!   `pub(in ...) `), equals a REMOVED line (same normalisation) from the same
//!   evaluated change set does not count. Multiset: each removed line cancels
//!   at most one added line. The change set is what tdd already evaluates
//!   (working tree + index + session commits).
//! * Lines that are only a `use ...;` statement (incl. multi-line use) or a
//!   `mod <name>;` declaration are not impl lines.
//! * Everything else is unchanged.
//!
//! DECISION (spec ambiguity): only removals from IMPL files can cancel an added
//! impl line. A line deleted from a docs/notes file is not code that moved, so
//! it must not vouch for the same text appearing in `src/` (CONTROL 4).
//!
//! Dates are pinned (UTC `Z`) like `committed_session_work_is_evaluated.rs`.
//! Isolation: `$HOME` is a per-test tempdir. Written by a disinterested author
//! (CLAUDE.md 2(a)).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

const SEED_DATE: &str = "2026-01-01T00:00:00Z";
const PRE_SESSION: &str = "2026-05-01T00:00:00Z";
const SESSION_START: &str = "2026-06-01T05:00:00.000Z";
const IN_SESSION_WRITE: &str = "2026-06-01T05:10:00.000Z";
const IN_SESSION_COMMIT: &str = "2026-06-01T05:20:00Z";
const SESSION_LAST: &str = "2026-06-01T05:30:00.000Z";

const USES: &str = "use std::collections::HashMap;\nuse std::fmt::Write as _;\n";

/// A ~20-line private fn with distinct lines (so the multiset has no accidental
/// duplicates other than braces).
const HELPER_PRIVATE: &str = "fn helper(input: &str) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    let mut buf = String::new();
    for word in input.split_whitespace() {
        let key = word.to_lowercase();
        let entry = counts.entry(key).or_insert(0);
        *entry += 1;
        buf.clear();
        write!(buf, \"{word}\").unwrap();
    }
    let total: usize = counts.values().sum();
    if total == 0 {
        return counts;
    }
    let longest = counts.keys().map(|k| k.len()).max().unwrap_or(0);
    if longest > 64 {
        counts.clear();
    }
    counts.shrink_to_fit();
    counts
}
";

/// The same fn with a visibility qualifier added (CONTROL: RED 3).
fn helper_with_vis(vis: &str) -> String {
    HELPER_PRIVATE.replacen("fn helper", &format!("{vis} fn helper"), 1)
}

struct Fixture {
    home: PathBuf,
    root: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    git_env(dir, args, &[]);
}

fn git_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) {
    let mut c = Command::new("git");
    harness_core::git_env::scrub(&mut c);
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

fn commit_at(dir: &Path, msg: &str, date: &str) {
    git(dir, &["add", "-A"]);
    git_env(
        dir,
        &["commit", "-qm", msg],
        &[("GIT_AUTHOR_DATE", date), ("GIT_COMMITTER_DATE", date)],
    );
}

fn stamp(mut v: serde_json::Value, ts: &str) -> serde_json::Value {
    v["timestamp"] = serde_json::json!(ts);
    v
}

impl Fixture {
    /// Single-root repo. `base` files are committed BEFORE the session starts.
    fn new(base: &[(&str, &str)]) -> Fixture {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!("tdd-pure-move-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let root = home.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.email", "t@t.com"]);
        git(&root, &["config", "user.name", "t"]);
        git(&root, &["config", "commit.gpgsign", "false"]);
        std::fs::write(root.join("seed.txt"), "seed\n").unwrap();
        commit_at(&root, "init", SEED_DATE);
        let root = root.canonicalize().unwrap();
        let f = Fixture {
            home: home.clone(),
            root,
        };
        for (rel, body) in base {
            f.put(rel, body);
        }
        commit_at(&f.root, "pre-session base", PRE_SESSION);
        std::fs::create_dir_all(home.join(".harness")).unwrap();
        std::fs::write(
            home.join(".harness/trust.toml"),
            format!("trusted = [\"{}\"]\n", f.root.display()),
        )
        .unwrap();
        f
    }

    fn put(&self, rel: &str, body: &str) -> PathBuf {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        p
    }

    fn commit_in_session(&self) {
        commit_at(&self.root, "session work", IN_SESSION_COMMIT);
    }

    /// Transcript: user prompt at session start, one Write per touched path
    /// (attribution), closing text record after the in-session commit.
    fn transcript(&self, written: &[&str]) -> PathBuf {
        let mut recs = vec![stamp(
            serde_json::json!({
                "type": "user", "uuid": "u0", "sessionId": "s-move",
                "message": {"role": "user", "content": "go"}
            }),
            SESSION_START,
        )];
        for rel in written {
            let p = self.root.join(rel);
            recs.push(stamp(
                serde_json::json!({
                    "type": "assistant", "parentUuid": "u0", "isSidechain": false,
                    "sessionId": "s-move",
                    "message": {"model": "claude-opus-5", "id": "m1", "type": "message",
                        "role": "assistant",
                        "content": [{"type": "tool_use", "id": "t1", "name": "Write",
                            "input": {"file_path": p.to_string_lossy(), "content": "x"}}]}
                }),
                IN_SESSION_WRITE,
            ));
        }
        recs.push(stamp(
            serde_json::json!({
                "type": "assistant", "parentUuid": "m1", "isSidechain": false,
                "sessionId": "s-move",
                "message": {"model": "claude-opus-5", "id": "m2", "type": "message",
                    "role": "assistant", "content": [{"type": "text", "text": "done"}]}
            }),
            SESSION_LAST,
        ));
        let d = self.home.join(".claude/projects/-slug");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("sess.jsonl");
        let mut body = String::new();
        for r in &recs {
            body.push_str(&r.to_string());
            body.push('\n');
        }
        std::fs::write(&p, body).unwrap();
        p
    }

    /// Run the real `tdd gate`; returns (stdout, stderr).
    fn stop(&self, transcript: &Path) -> (String, String) {
        let payload = serde_json::json!({
            "session_id": "s-move",
            "transcript_path": transcript.to_string_lossy(),
            "cwd": self.root.to_string_lossy(),
            "hook_event_name": "Stop",
            "stop_hook_active": false,
        })
        .to_string();
        let mut child = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_tdd")))
            .arg("gate")
            .current_dir(&self.root)
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_SESSION_ID", "s-move")
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

/// Pre-session base: `a.rs` holds the helper (with `use` lines), `lib.rs` is
/// just `mod a;`.
fn base_with_helper_in_a() -> Fixture {
    Fixture::new(&[
        ("src/lib.rs", "mod a;\n"),
        ("src/a.rs", &format!("{USES}\n{HELPER_PRIVATE}")),
    ])
}

/// Perform the move in the working tree: helper (rendered with `helper_text`)
/// goes to a new `src/b.rs` along with the `use` lines; `a.rs` keeps only
/// `extra_in_a`; `lib.rs` gains `mod b;`.
fn do_move(f: &Fixture, helper_text: &str, extra_in_a: &str) {
    f.put("src/a.rs", extra_in_a);
    f.put("src/b.rs", &format!("{USES}\n{helper_text}"));
    f.put("src/lib.rs", "mod a;\nmod b;\n");
}

const TOUCHED: [&str; 3] = ["src/a.rs", "src/b.rs", "src/lib.rs"];

// ---------------------------------------------------------------------------
// RED
// ---------------------------------------------------------------------------

/// RED 1: uncommitted pure move (fn + `use` lines to a new untracked file,
/// `mod b;` added) must ALLOW.
#[test]
fn uncommitted_pure_move_is_allowed() {
    let f = base_with_helper_in_a();
    do_move(&f, HELPER_PRIVATE, "");
    let tp = f.transcript(&TOUCHED);
    let (stdout, stderr) = f.stop(&tp);
    assert!(
        !blocks(&stdout),
        "a pure move is not new implementation and must be allowed; stdout={stdout:?} stderr={stderr:?}"
    );
}

/// RED 2: the same move, committed during the session (session-commit path).
#[test]
fn committed_pure_move_is_allowed() {
    let f = base_with_helper_in_a();
    do_move(&f, HELPER_PRIVATE, "");
    f.commit_in_session();
    let tp = f.transcript(&TOUCHED);
    let (stdout, stderr) = f.stop(&tp);
    assert!(
        !blocks(&stdout),
        "a committed pure move is not new implementation and must be allowed; stdout={stdout:?} stderr={stderr:?}"
    );
}

/// RED 3: move with a visibility change (`fn helper` -> `pub(super) fn helper`).
#[test]
fn move_with_visibility_change_is_allowed() {
    let f = base_with_helper_in_a();
    do_move(&f, &helper_with_vis("pub(super)"), "");
    let tp = f.transcript(&TOUCHED);
    let (stdout, stderr) = f.stop(&tp);
    assert!(
        !blocks(&stdout),
        "visibility qualifier must be ignored when matching moved lines; stdout={stdout:?} stderr={stderr:?}"
    );
}

// ---------------------------------------------------------------------------
// CONTROLS (green now and after the fix)
// ---------------------------------------------------------------------------

/// CONTROL 1: a brand-new fn with no test still blocks.
#[test]
fn control_new_fn_without_test_blocks() {
    let f = base_with_helper_in_a();
    f.put(
        "src/b.rs",
        "fn brand_new(n: i32) -> i32 {\n    let doubled = n * 2;\n    let shifted = doubled + 7;\n    shifted - 1\n}\n",
    );
    f.put("src/lib.rs", "mod a;\nmod b;\n");
    let tp = f.transcript(&["src/b.rs", "src/lib.rs"]);
    let (stdout, stderr) = f.stop(&tp);
    assert!(
        blocks(&stdout),
        "new untested fn must block; stdout={stdout:?} stderr={stderr:?}"
    );
}

/// CONTROL 2: a move plus ONE new logic line still blocks.
#[test]
fn control_move_plus_one_new_logic_line_blocks() {
    let f = base_with_helper_in_a();
    let tweaked = HELPER_PRIVATE.replacen(
        "    counts.shrink_to_fit();\n",
        "    counts.shrink_to_fit();\n    let x = 1;\n",
        1,
    );
    assert_ne!(tweaked, HELPER_PRIVATE);
    do_move(&f, &tweaked, "");
    let tp = f.transcript(&TOUCHED);
    let (stdout, stderr) = f.stop(&tp);
    assert!(
        blocks(&stdout),
        "a moved fn with an extra logic line must block; stdout={stdout:?} stderr={stderr:?}"
    );
}

/// CONTROL 3: a copy (original kept in a.rs) is new implementation: nothing
/// was removed, so nothing can cancel.
#[test]
fn control_copy_without_removal_blocks() {
    let f = base_with_helper_in_a();
    f.put("src/b.rs", &format!("{USES}\n{HELPER_PRIVATE}"));
    f.put("src/lib.rs", "mod a;\nmod b;\n");
    let tp = f.transcript(&["src/b.rs", "src/lib.rs"]);
    let (stdout, stderr) = f.stop(&tp);
    assert!(
        blocks(&stdout),
        "a duplicated fn (original kept) must block; stdout={stdout:?} stderr={stderr:?}"
    );
}

/// CONTROL 4: lines removed from a NON-impl file (docs) do not cancel the same
/// text added as impl. The fn text lived in `docs/snippet.txt`; the session
/// deletes it there and adds it to `src/b.rs`. Decision: only impl-file
/// removals cancel.
#[test]
fn control_removal_in_non_impl_file_does_not_cancel_added_impl() {
    let f = Fixture::new(&[
        ("src/lib.rs", "mod a;\n"),
        ("src/a.rs", "pub fn other() -> i32 {\n    40 + 2\n}\n"),
        ("docs/snippet.txt", HELPER_PRIVATE),
    ]);
    f.put("docs/snippet.txt", "snippet moved into the code\n");
    f.put("src/b.rs", &format!("{USES}\n{HELPER_PRIVATE}"));
    f.put("src/lib.rs", "mod a;\nmod b;\n");
    let tp = f.transcript(&["docs/snippet.txt", "src/b.rs", "src/lib.rs"]);
    let (stdout, stderr) = f.stop(&tp);
    assert!(
        blocks(&stdout),
        "a line removed from a docs file must not cancel added impl; stdout={stdout:?} stderr={stderr:?}"
    );
}
