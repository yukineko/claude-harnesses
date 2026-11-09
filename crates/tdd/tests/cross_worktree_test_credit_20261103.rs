// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! INTENTIONALLY RED reproduction (filed 2026-11-03, tdd 0.1.60).
//!
//! Observed in session c4aa3733 at 2026-11-03T10:44:15Z: the tdd Stop gate
//! blocked with
//!
//! ```text
//! 🔴 tdd: write a test first — 399 new implementation line(s) added, and no
//! test is visible in the uncommitted changes and this session's own commits
//! ... implementation changed: crates/fde/src/evidence.rs, crates/fde/src/gate.rs,
//! crates/fde/src/main.rs, crates/fde/src/ratify.rs, crates/fde/src/target.rs
//! ```
//!
//! while the session's integration branch already held RED test commits under
//! `crates/fde/tests`. Cause, traced from the transcript: the session's
//! subagents wrote each task in its own condukt worktree (14 of them, all still
//! live at the stop, removed ~16 min later). `session_roots` makes EVERY
//! checkout the session wrote into a separate root, and `gate::evaluate` judges
//! each root alone, with attribution keyed on that root's absolute paths. The
//! file list above is exactly the set the scaffold worker `Write`-ed in
//! `run-20261103-171621-35629-scaffold`, whose HEAD held only the scaffold
//! commit (437 added src lines, no test). The tests the same session wrote live
//! in sibling worktrees, so this root saw "no test". The reason also does not
//! name which checkout it judged, which is why the parent could only guess.
//!
//! Fixture (all in a temp `$HOME`): primary repo (hook cwd, as observed);
//! integration worktree `integ`; worktree `wt-red` where the session writes
//! and commits a test; worktree `wt-impl` where the session writes and commits
//! the implementation; both merged into `integ`. All commits are inside the
//! session window. Transcript records name the files at the worktree paths the
//! session actually wrote.
//!
//! Properties asserted (reading (a), see the backlog item):
//! 1. impl and its test both written and committed by THIS session, in sibling
//!    worktrees, must not yield "no test is visible";
//! 2. same, when the test commit is in the impl worktree's own HEAD history
//!    (impl worktree cut after the RED merge — the condukt impl-task shape);
//! 3. a missing-test block names the checkout it judged.
//!
//! Control: no test written anywhere still blocks with "no test is visible"
//! (the apparatus can produce the verdict at all).
//!
//! RE-SCOPED 2026-11-08 (user ruling 2026-11-08: gates judge own worktree only
//! — "donegateそのものが他のworktreeを見たり検証したりテストしたりすることは禁止
//! とする。他のgateでも禁止とする"). The cross-worktree credit described above
//! (properties 1 and 2 as "a test in a SIBLING worktree is credited") is
//! WITHDRAWN: the hook now runs with cwd = the checkout holding the
//! implementation and reads nothing else. Property 1 is inverted
//! (`test_written_in_a_sibling_worktree_is_not_credited`), property 2 is kept
//! for the cwd checkout's own HEAD history, and property 3 / the control are
//! kept with cwd = the implementation worktree (previously cwd = a clean
//! primary tree that the gate looked past).
//!
//! Written by a disinterested author (CLAUDE.md §2(a)); `src/` untouched.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

const IMPL: &str = "pub fn add(a: i32, b: i32) -> i32 {\n    let s = a + b;\n    s\n}\n";
const TEST: &str = "#[test]\nfn adds() {\n    assert_eq!(2 + 2, 4);\n}\n";
const SESSION_TS: &str = "2001-01-01T00:00:00Z";
const PRE_SESSION: &str = "2000-01-01T00:00:00+0000";

fn git_env(dir: &Path, args: &[&str], envs: &[(&str, &str)]) {
    let mut c = Command::new("git");
    harness_core::git_env::scrub(&mut c)
        .current_dir(dir)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (k, v) in envs {
        c.env(k, v);
    }
    let st = c.status().expect("git runs");
    assert!(st.success(), "git {args:?} failed in {}", dir.display());
}

fn git(dir: &Path, args: &[&str]) {
    git_env(dir, args, &[]);
}

fn commit(dir: &Path, msg: &str) {
    git(dir, &["-c", "commit.gpgsign=false", "commit", "-qm", msg]);
}

fn put(root: &Path, rel: &str, body: &str) -> PathBuf {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
    p
}

struct Fixture {
    home: PathBuf,
    primary: PathBuf,
}

struct Outcome {
    decision: String,
    reason: String,
    raw: String,
}

impl Fixture {
    fn new() -> Fixture {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!("tdd-xwt-credit-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let primary = home.join("primary");
        std::fs::create_dir_all(&primary).unwrap();
        git(&primary, &["init", "-q", "-b", "main"]);
        git(&primary, &["config", "user.email", "t@t.com"]);
        git(&primary, &["config", "user.name", "t"]);
        put(&primary, "README.md", "seed\n");
        git(&primary, &["add", "README.md"]);
        git_env(
            &primary,
            &["-c", "commit.gpgsign=false", "commit", "-qm", "seed"],
            &[
                ("GIT_AUTHOR_DATE", PRE_SESSION),
                ("GIT_COMMITTER_DATE", PRE_SESSION),
            ],
        );
        let primary = primary.canonicalize().unwrap();
        Fixture { home, primary }
    }

    fn worktree(&self, name: &str, base: &str) -> PathBuf {
        let wt = self.home.join(name);
        git(
            &self.primary,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                name,
                wt.to_str().unwrap(),
                base,
            ],
        );
        wt.canonicalize().unwrap()
    }

    fn trust(&self, roots: &[&Path]) {
        std::fs::create_dir_all(self.home.join(".harness")).unwrap();
        let list: Vec<String> = roots
            .iter()
            .map(|r| format!("\"{}\"", r.display()))
            .collect();
        std::fs::write(
            self.home.join(".harness/trust.toml"),
            format!("trusted = [{}]\n", list.join(", ")),
        )
        .unwrap();
    }

    fn transcript(&self, written: &[&Path]) -> PathBuf {
        let d = self.home.join(".claude/projects/-slug");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("sess.jsonl");
        let mut body = serde_json::json!({
            "type": "user", "uuid": "u0", "sessionId": "s-xwt", "timestamp": SESSION_TS,
            "message": {"role": "user", "content": "go"}
        })
        .to_string();
        body.push('\n');
        for w in written {
            body.push_str(
                &serde_json::json!({
                    "type": "assistant", "parentUuid": "u0", "isSidechain": false,
                    "sessionId": "s-xwt", "timestamp": SESSION_TS,
                    "message": {"model": "claude-opus-5", "id": "m1", "type": "message",
                        "role": "assistant",
                        "content": [{"type": "tool_use", "id": "t1", "name": "Write",
                            "input": {"file_path": w.to_string_lossy(), "content": "x"}}]}
                })
                .to_string(),
            );
            body.push('\n');
        }
        std::fs::write(&p, body).unwrap();
        p
    }

    /// Run `tdd gate` with the hook cwd = `cwd` (user ruling 2026-11-08: the
    /// gate judges only that checkout).
    fn stop(&self, cwd: &Path, transcript: &Path) -> Outcome {
        let payload = serde_json::json!({
            "session_id": "s-xwt",
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
            .env("CLAUDE_CODE_SESSION_ID", "s-xwt")
            .env_remove("TDD_DISABLE")
            .env_remove("HARNESS_TRUST_ALL")
            .env_remove("HARNESS_WORKTREE_ORIGIN_DIR")
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
            raw: format!(
                "stdout={stdout:?} stderr={:?}",
                String::from_utf8_lossy(&out.stderr)
            ),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn no_test_block(o: &Outcome) -> bool {
    o.decision == "block" && o.reason.contains("no test is visible")
}

/// Sibling worktrees: the scaffold/impl worktree never contains the test.
/// Returns (fixture, transcript, impl worktree).
fn sibling_layout(write_test: bool) -> (Fixture, PathBuf, PathBuf) {
    let f = Fixture::new();
    let integ = f.worktree("integ", "main");
    let red = f.worktree("wt-red", "integ");
    let imp = f.worktree("wt-impl", "integ");
    let mut written = Vec::new();
    if write_test {
        written.push(put(&red, "tests/adds.rs", TEST));
        git(&red, &["add", "-A"]);
        commit(&red, "RED test");
    }
    written.push(put(&imp, "src/lib.rs", IMPL));
    git(&imp, &["add", "-A"]);
    commit(&imp, "impl");
    if write_test {
        git(&integ, &["merge", "-q", "--no-edit", "wt-red"]);
    }
    git(&integ, &["merge", "-q", "--no-edit", "wt-impl"]);
    f.trust(&[&f.primary, &integ, &red, &imp]);
    let refs: Vec<&Path> = written.iter().map(PathBuf::as_path).collect();
    let tp = f.transcript(&refs);
    (f, tp, imp)
}

/// CONTROL (anti-vacuity), re-scoped 2026-11-08 (user ruling 2026-11-08: gates
/// judge own worktree only): hook cwd = the impl worktree (was: the clean
/// primary). The session wrote implementation and no test anywhere — tdd must
/// block with "no test is visible".
#[test]
fn control_no_test_written_anywhere_blocks() {
    let (f, tp, imp) = sibling_layout(false);
    let o = f.stop(&imp, &tp);
    assert!(no_test_block(&o), "apparatus must block: {}", o.raw);
}

/// INVERTED (user ruling 2026-11-08: gates judge own worktree only). Was
/// `test_written_by_this_session_in_a_sibling_worktree_is_credited` (the
/// 2026-11-03 cross-worktree credit; passed only vacuously once cwd was the
/// clean primary). The session's committed test lives in sibling wt-red, the
/// impl in wt-impl (cwd, cut BEFORE the RED merge so its HEAD history lacks the
/// test): the sibling is not read, so the block stays and nothing is credited.
#[test]
fn test_written_in_a_sibling_worktree_is_not_credited() {
    let (f, tp, imp) = sibling_layout(true);
    let o = f.stop(&imp, &tp);
    assert!(
        no_test_block(&o),
        "a test committed in a SIBLING worktree must not lift the block: {}",
        o.raw
    );
    assert!(
        !o.raw.contains("credited with"),
        "must not claim a cross-worktree credit: {}",
        o.raw
    );
}

/// The condukt impl-task shape: the impl worktree is cut AFTER the RED merge,
/// so the session's test commit is in the impl worktree's own HEAD history.
/// Shared fixture: returns (fixture, red worktree test path, impl worktree,
/// impl file path).
fn red_merged_before_impl_cut() -> (Fixture, PathBuf, PathBuf, PathBuf) {
    let f = Fixture::new();
    let integ = f.worktree("integ", "main");
    let red = f.worktree("wt-red", "integ");
    let t = put(&red, "tests/adds.rs", TEST);
    git(&red, &["add", "-A"]);
    commit(&red, "RED test");
    git(&integ, &["merge", "-q", "--ff-only", "wt-red"]);
    let imp = f.worktree("wt-impl", "integ");
    let i = put(&imp, "src/lib.rs", IMPL);
    git(&imp, &["add", "-A"]);
    commit(&imp, "impl");
    git(&integ, &["merge", "-q", "--ff-only", "wt-impl"]);
    f.trust(&[&f.primary, &integ, &red, &imp]);
    (f, t, imp, i)
}

/// Re-scoped 2026-11-08 (user ruling 2026-11-08: gates judge own worktree
/// only): hook cwd = wt-impl, whose OWN history holds the test commit, and the
/// transcript shows the session writing that test AT wt-impl's path (attribution
/// is keyed on the checkout's own paths; the test is credited as the cwd
/// checkout's own committed session test).
#[test]
fn session_test_commit_in_the_impl_worktree_history_is_credited() {
    let (f, _t_at_red, imp, i) = red_merged_before_impl_cut();
    let t_here = imp.join("tests/adds.rs");
    assert!(t_here.exists(), "apparatus: the RED test is in wt-impl");
    let tp = f.transcript(&[&t_here, &i]);
    let o = f.stop(&imp, &tp);
    assert!(
        !no_test_block(&o),
        "the session's own test commit is in wt-impl's HEAD history, yet tdd \
         reported no test visible: {}",
        o.raw
    );
}

/// NEW (user ruling 2026-11-08: gates judge own worktree only). Same history,
/// but the transcript shows the session writing the test only in the SIBLING
/// wt-red (the original cross-worktree shape). The cwd checkout cannot attribute
/// that file to the session by looking at wt-red, so it is not credited and the
/// block stands. (Consequence of the ruling for condukt-style flows: the impl
/// worktree's stop is blocked unless the test is also written there or
/// `tdd skip` is used; the cross-worktree credit is withdrawn.)
#[test]
fn test_written_only_in_sibling_path_is_not_attributed_in_cwd() {
    let (f, t_at_red, imp, i) = red_merged_before_impl_cut();
    let tp = f.transcript(&[&t_at_red, &i]);
    let o = f.stop(&imp, &tp);
    assert!(
        no_test_block(&o),
        "a test the transcript places in a sibling worktree must not credit wt-impl: {}",
        o.raw
    );
    assert!(!o.raw.contains("credited with"), "{}", o.raw);
}

/// A missing-test block must say WHICH checkout it judged: with several roots
/// the reader cannot otherwise tell a stale sibling worktree from the branch
/// they are looking at. Re-scoped 2026-11-08 (user ruling 2026-11-08: gates
/// judge own worktree only): hook cwd = the impl worktree, which the reason names.
#[test]
fn missing_test_block_names_the_checkout_it_judged() {
    let (f, tp, imp) = sibling_layout(false);
    let o = f.stop(&imp, &tp);
    assert!(no_test_block(&o), "apparatus must block: {}", o.raw);
    assert!(
        o.reason.contains(&imp.display().to_string()),
        "the block judged {} but its reason does not name it: {:?}",
        imp.display(),
        o.reason
    );
}
