// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Fail-closed side of the test credit. Originally the per-session,
//! cross-worktree credit (user ruling 2026-11-03, backlog 49e95d32; tdd
//! 0.1.62); RE-SCOPED 2026-11-08 by the user ruling 2026-11-08: gates judge
//! own worktree only ("donegateそのものが他のworktreeを見たり検証したりテスト
//! したりすることは禁止とする"). The Stop hook now judges ONLY the cwd
//! checkout; it never reads sibling worktrees, so no sibling test can credit
//! it and no sibling's broken git can make the verdict undetermined.
//!
//! Each hook run here uses cwd = the checkout holding the implementation
//! (`wt-impl`, or `wt-a`). Tests pin: unreadable transcript never credits, a
//! pre-session / uncommitted / sibling-only test never credits, the reason
//! names only the checkout it judged, and a corrupt OTHER worktree neither
//! blocks nor changes the verdict (while a red cwd still blocks).
//! Written by a disinterested author (CLAUDE.md 2(a)).

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
        red,
        imp,
        test_file,
        impl_file,
    }
}

/// Case 1 (note, user ruling 2026-11-08: gates judge own worktree only — the
/// "sibling" test below is not read at all now; the property that survives is
/// that an unreadable transcript yields a block and no claimed credit):
/// Credit needs verified attribution. The impl is left uncommitted in
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

/// Case 2: INVERTED/RE-SCOPED (user ruling 2026-11-08: gates judge own worktree
/// only). Was `undeterminable_session_root_blocks_instead_of_crediting`: a
/// sibling root with a corrupt index made the session-wide evidence
/// undetermined and withheld credit ("NOT credited"). Sibling roots are no
/// longer read, so a corrupt OTHER worktree (wt-dead, named by the transcript,
/// index garbage) must change nothing:
///  * (a)/(b) cwd = wt-impl holding its OWN committed session test + impl: the
///    verdict is allow, identical to the healthy run, with no undetermined block;
///  * (c) the same layout with the test removed from wt-impl: still BLOCKS
///    ("no test is visible"), so the allow above is not a vacuous pass.
#[test]
fn corrupt_other_worktree_does_not_change_the_verdict() {
    let f = Fixture::new();
    let integ = f.worktree("integ", "main");
    let imp = f.worktree("wt-impl", "integ");
    let dead = f.worktree("wt-dead", "integ");
    let d = put(&dead, "src/other.rs", IMPL);
    let t = put(&imp, "tests/adds.rs", TEST);
    let i = put(&imp, "src/lib.rs", IMPL);
    git(&imp, &["add", "-A"]);
    commit(&imp, "impl + test");
    f.trust(&[&f.primary, &integ, &imp, &dead]);
    let tp = f.transcript(&[&t, &i, &d]);

    let healthy = f.stop_in(&tp, &imp);
    assert!(
        healthy.decision != "block",
        "control: own committed test + impl must be accepted: {}",
        healthy.raw
    );

    // wt-dead still resolves (rev-parse works) but its git scan would fail.
    std::fs::write(
        f.primary.join(".git/worktrees/wt-dead").join("index"),
        "not an index\n",
    )
    .unwrap();
    let o = f.stop_in(&tp, &imp);
    assert!(
        o.decision != "block",
        "a corrupt OTHER worktree must not block: {}",
        o.raw
    );
    assert!(
        !o.reason.contains("wt-dead") && !o.reason.contains("NOT credited"),
        "{}",
        o.raw
    );

    // (c) anti-vacuity: drop the test from wt-impl's HEAD; the cwd is red again.
    git(&imp, &["rm", "-q", "tests/adds.rs"]);
    commit(&imp, "drop test");
    let tp2 = f.transcript(&[&i, &d]);
    let red = f.stop_in(&tp2, &imp);
    assert!(
        no_test_block(&red),
        "red cwd must still block with a corrupt neighbour: {}",
        red.raw
    );
}

/// Case 3: A test committed BEFORE the session started is not the session's
/// work and must not be credited. RE-SCOPED (user ruling 2026-11-08: gates
/// judge own worktree only): was a pre-session test in wt-red (a sibling); the
/// sibling is no longer read, so the pre-session test is now committed in
/// wt-impl itself (the only checkout judged) and the hook runs there.
#[test]
fn pre_session_test_commit_in_cwd_is_not_credited() {
    let f = Fixture::new();
    let integ = f.worktree("integ", "main");
    let imp = f.worktree("wt-impl", "integ");
    let t = put(&imp, "tests/adds.rs", TEST);
    git(&imp, &["add", "-A"]);
    git_env(
        &imp,
        &["-c", "commit.gpgsign=false", "commit", "-qm", "old test"],
        &[
            ("GIT_AUTHOR_DATE", PRE_SESSION),
            ("GIT_COMMITTER_DATE", PRE_SESSION),
        ],
    );
    let i = put(&imp, "src/lib.rs", IMPL);
    git(&imp, &["add", "-A"]);
    commit(&imp, "impl");
    f.trust(&[&f.primary, &integ, &imp]);
    let tp = f.transcript(&[&t, &i]);
    let o = f.stop_in(&tp, &imp);
    assert!(
        no_test_block(&o),
        "a pre-session test must not lift the block: {}",
        o.raw
    );
    assert!(o.reason.contains(&imp.display().to_string()), "{}", o.raw);
}

/// Case 3b: NEW (user ruling 2026-11-08: gates judge own worktree only). The
/// session's committed test lives in a SIBLING worktree (wt-red), the
/// implementation in wt-impl (cwd). Was the credited case (cross-worktree
/// credit, 2026-11-03); now the sibling test is not read, so it does not lift
/// the block, and the reason says nothing was credited.
#[test]
fn committed_test_in_sibling_worktree_is_not_credited() {
    let l = layout(None, true, true);
    let tp = l.f.transcript(&[&l.test_file, &l.impl_file]);
    let o = l.f.stop_in(&tp, &l.imp);
    assert!(
        no_test_block(&o),
        "a sibling worktree's test must not lift the block: {}",
        o.raw
    );
    assert!(
        !o.raw.contains("credited with") && !o.reason.contains(&l.red.display().to_string()),
        "must not claim or name the sibling's credit: {}",
        o.raw
    );
}

/// Case 4: Two roots both lack tests: the block reason names the root it judged
/// (the cwd, wt-a) and not the other one. RE-SCOPED (user ruling 2026-11-08):
/// the hook runs in wt-a, so the judged checkout is deterministically wt-a and
/// wt-b is never named (was: whichever of the two roots was evaluated first).
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
    let o = f.stop_in(&tp, &a);
    assert!(no_test_block(&o), "apparatus must block: {}", o.raw);
    let (sa, sb) = (a.display().to_string(), b.display().to_string());
    let (na, nb) = (o.reason.contains(&sa), o.reason.contains(&sb));
    assert!(
        na && !nb,
        "reason must name exactly the one root it judged, wt-a (a={na}, b={nb}): {:?}",
        o.reason
    );
    let judged = o
        .reason
        .split("Checkout judged: ")
        .nth(1)
        .unwrap_or_else(|| panic!("no 'Checkout judged:' line: {:?}", o.reason));
    assert!(judged.starts_with(&sa), "{judged:?}");
}

/// Case 5: the ruling credits a test the session "wrote and committed". A test
/// left UNCOMMITTED in a sibling root must not credit wt-impl's implementation.
/// RE-SCOPED (user ruling 2026-11-08: gates judge own worktree only): the hook
/// now runs with cwd = wt-impl (it used to run in the primary tree and fold
/// every root); a sibling's uncommitted test is not read either way.
#[test]
fn uncommitted_sibling_test_is_not_credited() {
    let l = layout(None, false, true);
    let tp = l.f.transcript(&[&l.test_file, &l.impl_file]);
    let o = l.f.stop_in(&tp, &l.imp);
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
