#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (2026-11-16, observed live in session 06c2bd77): the Stop gate
//! printed "10 uncommitted file(s) not attributed to this session were
//! excluded: .backlog/tasks.done.toml, .specguard/spec-docs.toml,
//! crates/blastguard/..." while `git status --short` in the judged checkout was
//! EMPTY.
//!
//! Diff source (crates/tdd/src/gate.rs, read at main 9a989926b):
//! `evaluate` -> `include_committed` folds the added lines of every NON-MERGE
//! commit on HEAD committed since the session start (peers' commits merged
//! into main included) into the scan; `scope_to_session` then attributes the
//! union and the files it drops become `Authorship::Attributed { excluded }`.
//! `attribution_note` / `generic_block_reason` print that list as
//! "uncommitted file(s) not attributed to this session", but those files are
//! COMMITTED (a peer's commit), not uncommitted.
//!
//! Reading (a), asserted: the prose names a committed peer file as
//! "uncommitted" with a clean working tree — a docstring/message vs actual
//! behaviour mismatch (CLAUDE.md 4). Reading (b): the exclusion itself is
//! correct (peer work is not this session's), only the word is loose. Both
//! readings agree the exclusion is correct; this test fails only on the
//! label, so a fix that relabels (e.g. "committed or uncommitted") passes it.
//!
//! Does NOT prove: anything about the "implementation changed: proof.rs" line
//! of the same observation (that is this session's own committed work, folded
//! in by design, ruling 2026-10-27 "Hybrid"), nor about merge commits.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SESSION_START: &str = "2026-06-01T05:00:00.000Z";
const IN_SESSION_WRITE: &str = "2026-06-01T05:10:00.000Z";
const PEER_COMMIT: &str = "2026-06-01T05:15:00Z";
const IN_SESSION_COMMIT: &str = "2026-06-01T05:20:00Z";
const SESSION_LAST: &str = "2026-06-01T05:30:00.000Z";

fn git(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let mut c = Command::new("git");
    harness_core::git_env::scrub(&mut c);
    c.current_dir(dir).args(args);
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c.output().expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed in {}",
        dir.display()
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn commit_at(dir: &Path, msg: &str, date: &str) {
    git(dir, &["add", "-A"], &[]);
    git(
        dir,
        &["commit", "-qm", msg],
        &[("GIT_AUTHOR_DATE", date), ("GIT_COMMITTER_DATE", date)],
    );
}

fn rec(v: serde_json::Value, ts: &str) -> String {
    let mut v = v;
    v["timestamp"] = serde_json::json!(ts);
    v.to_string()
}

// test-weakening-justified: new RED for an unfixed defect (backlog pending, 2026-11-16); the ignore is removed in the fixing commit.
#[ignore]
#[test]
fn committed_peer_file_is_not_reported_as_uncommitted() {
    let home = std::env::temp_dir().join(format!("tdd-peer-label-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let repo = home.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"], &[]);
    git(&repo, &["config", "user.email", "t@t.com"], &[]);
    git(&repo, &["config", "user.name", "t"], &[]);
    git(&repo, &["config", "commit.gpgsign", "false"], &[]);
    std::fs::write(repo.join("seed.txt"), "seed\n").unwrap();
    commit_at(&repo, "init", "2026-01-01T00:00:00Z");
    let repo: PathBuf = repo.canonicalize().unwrap();
    std::fs::create_dir_all(home.join(".harness")).unwrap();
    std::fs::write(
        home.join(".harness/trust.toml"),
        format!("trusted = [\"{}\"]\n", repo.display()),
    )
    .unwrap();

    // A peer's non-merge commit inside the session window.
    std::fs::create_dir_all(repo.join("peer")).unwrap();
    std::fs::write(
        repo.join("peer/other.rs"),
        "pub fn peer() -> u8 {\n    7\n}\n",
    )
    .unwrap();
    commit_at(&repo, "peer work", PEER_COMMIT);

    // This session's own impl + test, committed in the session.
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(repo.join("tests")).unwrap();
    let lib = repo.join("src/lib.rs");
    let t = repo.join("tests/t.rs");
    std::fs::write(&lib, "pub fn f() -> i32 {\n    let x = 1;\n    x + 1\n}\n").unwrap();
    std::fs::write(&t, "#[test]\nfn f_works() {\n    assert_eq!(2, 2);\n}\n").unwrap();
    commit_at(&repo, "session work", IN_SESSION_COMMIT);

    // Control: the judged checkout has NO uncommitted change.
    let status = git(&repo, &["status", "--porcelain"], &[]);
    assert_eq!(status, "", "control: working tree must be clean");

    let write = |p: &Path, id: &str| {
        serde_json::json!({
            "type": "assistant", "parentUuid": "u0", "isSidechain": false,
            "sessionId": "s-peer",
            "message": {"model": "claude-opus-5", "id": id, "type": "message",
                "role": "assistant",
                "content": [{"type": "tool_use", "id": id, "name": "Write",
                    "input": {"file_path": p.to_string_lossy(), "content": "x"}}]}
        })
    };
    let recs = [
        rec(
            serde_json::json!({"type": "user", "uuid": "u0", "sessionId": "s-peer",
                "message": {"role": "user", "content": "go"}}),
            SESSION_START,
        ),
        rec(write(&lib, "m1"), IN_SESSION_WRITE),
        rec(write(&t, "m2"), IN_SESSION_WRITE),
        rec(
            serde_json::json!({"type": "assistant", "parentUuid": "m2", "isSidechain": false,
                "sessionId": "s-peer",
                "message": {"model": "claude-opus-5", "id": "m3", "type": "message",
                    "role": "assistant", "content": [{"type": "text", "text": "done"}]}}),
            SESSION_LAST,
        ),
    ];
    let d = home.join(".claude/projects/-slug");
    std::fs::create_dir_all(&d).unwrap();
    let transcript = d.join("sess.jsonl");
    std::fs::write(&transcript, recs.join("\n") + "\n").unwrap();

    let payload = serde_json::json!({
        "session_id": "s-peer",
        "transcript_path": transcript.to_string_lossy(),
        "cwd": repo.to_string_lossy(),
        "hook_event_name": "Stop",
        "stop_hook_active": false,
    })
    .to_string();
    let mut child = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_tdd")))
        .arg("gate")
        .current_dir(&repo)
        .env("HOME", &home)
        .env("CLAUDE_CODE_SESSION_ID", "s-peer")
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
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = std::fs::remove_dir_all(&home);

    let all = format!("{stdout}\n{stderr}");
    let mislabelled = all
        .lines()
        .any(|l| l.contains("uncommitted file(s) not attributed") && l.contains("peer/other.rs"));
    assert!(
        !mislabelled,
        "a COMMITTED peer file was reported as an uncommitted file with a clean working tree \
         (git status --porcelain empty).\nstdout: {stdout}\nstderr: {stderr}"
    );
}
