#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Fail-open holes in the "a pure move is not new implementation" rule, found
//! by adversarial review of commit 2c2252ae. Behaviour encoded (not mechanism):
//!
//! * RED 1: in Python, indentation is control flow. Dedenting `return y` out of
//!   an `if` changes behaviour, so it is a modification, not a move: BLOCK.
//! * RED 2/3: an edit (`return false;` -> `return true;` in `fn f`) must not be
//!   vouched for by an UNRELATED deleted fn (`fn g` holding `return true;`),
//!   uncommitted (RED 2) or committed in-session (RED 3): BLOCK.
//! * Controls: a verbatim Python move at the same indentation, and a short
//!   (3 non-blank line) Rust fn move, are still ALLOWed; the RED 2 edit with
//!   nothing deleted still BLOCKs.
//!
//! Harness copied from `pure_move_not_new_impl.rs`. Written by a disinterested
//! author (CLAUDE.md 2(a)).

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
        let home = std::env::temp_dir().join(format!("tdd-pm-failopen-{}-{n}", std::process::id()));
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

const PY_BASE: &str = "def f(xs):\n    for x in xs:\n        if x:\n            y = x\n            return y\n    return None\n";
const PY_DEDENTED: &str = "def f(xs):\n    for x in xs:\n        if x:\n            y = x\n        return y\n    return None\n";

const RS_BASE: &str = "fn f(x: bool) -> bool {\n    if x {\n        return false;\n    }\n    true\n}\nfn g() -> bool {\n    return true;\n}\n";
const RS_EDIT_DELETE_G: &str =
    "fn f(x: bool) -> bool {\n    if x {\n        return true;\n    }\n    true\n}\n";
const RS_EDIT_KEEP_G: &str = "fn f(x: bool) -> bool {\n    if x {\n        return true;\n    }\n    true\n}\nfn g() -> bool {\n    return true;\n}\n";

fn assert_blocks(f: &Fixture, written: &[&str], why: &str) {
    let tp = f.transcript(written);
    let (stdout, stderr) = f.stop(&tp);
    assert!(
        blocks(&stdout),
        "{why}; stdout={stdout:?} stderr={stderr:?}"
    );
}

fn assert_allows(f: &Fixture, written: &[&str], why: &str) {
    let tp = f.transcript(written);
    let (stdout, stderr) = f.stop(&tp);
    assert!(
        !blocks(&stdout),
        "{why}; stdout={stdout:?} stderr={stderr:?}"
    );
}

/// RED 1: Python dedent out of an `if` changes control flow; must block.
#[test]
fn python_dedent_changing_control_flow_blocks() {
    let f = Fixture::new(&[("app/a.py", PY_BASE)]);
    f.put("app/a.py", PY_DEDENTED);
    assert_blocks(
        &f,
        &["app/a.py"],
        "dedenting `return y` out of the `if` changes behaviour and must block",
    );
}

/// RED 2: unrelated deletion must not vouch for an edit (uncommitted).
#[test]
fn unrelated_deletion_does_not_vouch_for_edit_uncommitted() {
    let f = Fixture::new(&[("src/a.rs", RS_BASE)]);
    f.put("src/a.rs", RS_EDIT_DELETE_G);
    assert_blocks(&f, &["src/a.rs"], "`return false;` -> `return true;` must block even though an unrelated fn with `return true;` was deleted");
}

/// RED 3: same, both changes committed in the session.
#[test]
fn unrelated_deletion_does_not_vouch_for_edit_committed() {
    let f = Fixture::new(&[("src/a.rs", RS_BASE)]);
    f.put("src/a.rs", RS_EDIT_DELETE_G);
    f.commit_in_session();
    assert_blocks(&f, &["src/a.rs"], "committed `return false;` -> `return true;` must block even though an unrelated fn with `return true;` was deleted");
}

/// CONTROL A: top-level Python fn moved verbatim, same indentation -> allow.
#[test]
fn control_python_verbatim_move_is_allowed() {
    let func = "def tally(items):\n    counts = {}\n    for item in items:\n        key = item.lower()\n        counts[key] = counts.get(key, 0) + 1\n    total = sum(counts.values())\n    return counts, total\n";
    let f = Fixture::new(&[("app/a.py", func)]);
    f.put("app/a.py", "");
    f.put("app/b.py", func);
    assert_allows(
        &f,
        &["app/a.py", "app/b.py"],
        "a verbatim Python move at the same indentation is not new implementation",
    );
}

/// CONTROL B: a 3-non-blank-line Rust fn moved to a new module -> allow.
#[test]
fn control_short_rust_fn_move_is_allowed() {
    let func = "fn h() -> u8 {\n    7\n}\n";
    let f = Fixture::new(&[("src/lib.rs", "mod a;\n"), ("src/a.rs", func)]);
    f.put("src/a.rs", "");
    f.put("src/b.rs", func);
    f.put("src/lib.rs", "mod a;\nmod b;\n");
    assert_allows(
        &f,
        &["src/a.rs", "src/b.rs", "src/lib.rs"],
        "a short fn moved with `mod b;` is not new implementation",
    );
}

/// CONTROL C: the RED 2 edit with `fn g` kept (nothing deleted) must block.
#[test]
fn control_edit_without_deletion_blocks() {
    let f = Fixture::new(&[("src/a.rs", RS_BASE)]);
    f.put("src/a.rs", RS_EDIT_KEEP_G);
    assert_blocks(
        &f,
        &["src/a.rs"],
        "`return false;` -> `return true;` with nothing deleted must block",
    );
}
