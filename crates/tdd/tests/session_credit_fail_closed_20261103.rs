// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Fail-closed side of the per-session test credit (user ruling 2026-11-03,
//! backlog 49e95d32; tdd 0.1.62). Companion to
//! `cross_worktree_test_credit_20261103.rs`, which pins the credited cases.
//!
//! Each test here pins a case where credit must NOT be given (or must be
//! attributed correctly): unreadable transcript, an undeterminable sibling root,
//! a pre-session test commit, an uncommitted sibling test, and the reason naming
//! the root it judged. Written by a disinterested author (CLAUDE.md 2(a)).

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
        let home =
            std::env::temp_dir().join(format!("tdd-sess-failclosed-{}-{n}", std::process::id()));
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

    fn stop(&self, transcript: &Path) -> Outcome {
        self.stop_in(transcript, &self.primary)
    }

    fn stop_in(&self, transcript: &Path, cwd: &Path) -> Outcome {
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

struct Layout {
    f: Fixture,
    integ: PathBuf,
    red: PathBuf,
    imp: PathBuf,
    test_file: PathBuf,
    impl_file: PathBuf,
}

/// integ, wt-red (holds the test), wt-impl (impl, no test). The test in wt-red
/// is committed with `test_date` (None = now, i.e. inside the session window),
/// or left uncommitted when `commit_test` is false.
fn layout(test_date: Option<&str>, commit_test: bool, commit_impl: bool) -> Layout {
    let f = Fixture::new();
    let integ = f.worktree("integ", "main");
    let red = f.worktree("wt-red", "integ");
    let imp = f.worktree("wt-impl", "integ");
    let test_file = put(&red, "tests/adds.rs", TEST);
    if commit_test {
        git(&red, &["add", "-A"]);
        match test_date {
            Some(d) => git_env(
                &red,
                &["-c", "commit.gpgsign=false", "commit", "-qm", "RED test"],
                &[("GIT_AUTHOR_DATE", d), ("GIT_COMMITTER_DATE", d)],
            ),
            None => commit(&red, "RED test"),
        }
    }
    let impl_file = put(&imp, "src/lib.rs", IMPL);
    if commit_impl {
        git(&imp, &["add", "-A"]);
        commit(&imp, "impl");
    }
    f.trust(&[&f.primary, &integ, &red, &imp]);
    Layout {
        f,
        integ,
        red,
        imp,
        test_file,
        impl_file,
    }
}

/// Case 1: Credit needs verified attribution. The impl is left uncommitted in
/// wt-impl (so it is scanned even without a transcript); hook runs there; the transcript
/// is missing / unreadable, so the sibling's test cannot be shown to be this
/// session's. Must still block, and must not claim a credit.
#[test]
fn unreadable_transcript_never_credits_a_sibling_test() {
    let l = layout(None, true, false);
    let missing = l.f.home.join("no-such-transcript.jsonl");
    let o = l.f.stop_in(&missing, &l.imp);
    assert_eq!(
        o.decision, "block",
        "missing transcript must not pass: {}",
        o.raw
    );
    assert!(
        !o.reason.contains("credited with"),
        "reason claims credit: {}",
        o.raw
    );

    // A directory where the transcript should be: present but unreadable as a file.
    let dir = l.f.home.join("transcript-is-a-dir");
    std::fs::create_dir_all(&dir).unwrap();
    let o = l.f.stop_in(&dir, &l.imp);
    assert_eq!(
        o.decision, "block",
        "unreadable transcript must not pass: {}",
        o.raw
    );
    assert!(
        !o.reason.contains("credited with"),
        "reason claims credit: {}",
        o.raw
    );
}

/// Case 2: A session root whose git cannot be scanned makes the session-wide
/// evidence undetermined: the test in the readable sibling must not be credited.
/// wt-dead is named by the transcript, then its index is corrupted so the
/// root still resolves but its git scan fails. The same session with wt-dead healthy IS credited
/// (control: the apparatus can produce the credited verdict).
#[test]
fn undeterminable_session_root_blocks_instead_of_crediting() {
    let l = layout(None, true, true);
    let dead = l.f.worktree("wt-dead", "integ");
    let d = put(&dead, "src/other.rs", IMPL);
    l.f.trust(&[&l.f.primary, &l.integ, &l.red, &l.imp, &dead]);
    let tp = l.f.transcript(&[&l.test_file, &l.impl_file, &d]);

    let gitdir = l.f.primary.join(".git/worktrees/wt-dead");
    let ok = l.f.stop(&tp);
    assert!(
        !no_test_block(&ok),
        "control: with every root healthy the sibling test must be credited: {}",
        ok.raw
    );

    // The root still resolves (rev-parse works) but its git scan fails.
    std::fs::write(gitdir.join("index"), "not an index\n").unwrap();
    let o = l.f.stop(&tp);
    assert_eq!(
        o.decision, "block",
        "an unscannable root must not pass: {}",
        o.raw
    );
    assert!(
        !o.reason.contains("credited with"),
        "reason claims credit: {}",
        o.raw
    );
    // wt-impl (a readable root with no test of its own) is the one reported, and
    // the reason says the sibling test was NOT credited because the session-wide
    // evidence is undetermined — not that it was credited or that nothing exists.
    assert!(
        o.reason.contains("NOT credited"),
        "reason must say the sibling credit was withheld: {}",
        o.raw
    );
}

/// Case 3: A test committed in the sibling BEFORE the session started is not the
/// session's work and must not be credited.
#[test]
fn pre_session_test_commit_in_sibling_is_not_credited() {
    let l = layout(Some(PRE_SESSION), true, true);
    let tp = l.f.transcript(&[&l.test_file, &l.impl_file]);
    let o = l.f.stop(&tp);
    assert!(
        no_test_block(&o),
        "a pre-session test must not lift the block: {}",
        o.raw
    );
    assert!(o.reason.contains(&l.imp.display().to_string()), "{}", o.raw);
}

/// Case 4: Two roots both lack tests: the block reason names the root it judged and
/// not the other one.
#[test]
fn block_reason_names_only_the_root_it_judged() {
    let f = Fixture::new();
    let integ = f.worktree("integ", "main");
    let a = f.worktree("wt-a", "integ");
    let b = f.worktree("wt-b", "integ");
    let ia = put(&a, "src/a.rs", IMPL);
    git(&a, &["add", "-A"]);
    commit(&a, "impl a");
    let ib = put(&b, "src/b.rs", IMPL);
    git(&b, &["add", "-A"]);
    commit(&b, "impl b");
    f.trust(&[&f.primary, &integ, &a, &b]);
    let tp = f.transcript(&[&ia, &ib]);
    let o = f.stop(&tp);
    assert!(no_test_block(&o), "apparatus must block: {}", o.raw);
    let (sa, sb) = (a.display().to_string(), b.display().to_string());
    let (na, nb) = (o.reason.contains(&sa), o.reason.contains(&sb));
    assert!(
        na ^ nb,
        "reason must name exactly the one root it judged (a={na}, b={nb}): {:?}",
        o.reason
    );
    let judged = o
        .reason
        .split("Checkout judged: ")
        .nth(1)
        .unwrap_or_else(|| panic!("no 'Checkout judged:' line: {:?}", o.reason));
    assert!(
        judged.starts_with(&sa) || judged.starts_with(&sb),
        "{judged:?}"
    );
}

/// Case 5: the ruling credits a test the session "wrote and committed". A test
/// left UNCOMMITTED in a sibling root must not credit wt-impl's implementation.
/// (Intentionally RED on tdd 0.1.62: root_test_evidence counts uncommitted files.)
#[test]
fn uncommitted_sibling_test_is_not_credited() {
    let l = layout(None, false, true);
    let tp = l.f.transcript(&[&l.test_file, &l.impl_file]);
    let o = l.f.stop(&tp);
    assert!(
        no_test_block(&o),
        "an uncommitted sibling test must not credit: {}",
        o.raw
    );
    assert!(
        !o.reason.contains("credited with") && !o.raw.contains("credited with"),
        "claims credit: {}",
        o.raw
    );
}
