#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Positive assertions on the committed/uncommitted wording of the tdd Stop
//! gate's "excluded peer files" message (backlog 9e565d01).
//!
//! The gate (`gate::attribution_note`, stderr on every run, and
//! `gate::generic_block_reason`, in a block reason) prints
//! `"{counts}, not attributed to this session, were excluded..."`, with
//! `{counts}` from `excluded_label::counts()`: `"N committed file(s)"`,
//! `"M uncommitted file(s)"`, or `"N committed file(s) and M uncommitted file(s)"`.
//! The pre-existing test `backlog_20261116_committed_peer_files_labelled_uncommitted.rs`
//! only asserts the ABSENCE of the old literal, which a comma alone satisfies;
//! these tests assert the PRESENCE of the exact intended phrase.
//!
//! What it pins, by running the real `tdd gate` binary on a real git repo:
//! 1. `committed_peer_file_only` — a peer's file known only from a commit in the
//!    session window: every occurrence of the excluded-files phrase is preceded
//!    by exactly `"1 committed file(s)"` (no `uncommitted` count), and the
//!    stderr note's file list is exactly `peer/other.rs`.
//! 2. `uncommitted_peer_file_only` — a peer's untracked working-tree file: the
//!    phrase is preceded by exactly `"1 uncommitted file(s)"`, no bare
//!    `"committed file(s)"` (one not preceded by `un`) appears anywhere, and the
//!    note's file list is exactly `peer/peer_uncommitted.rs`.
//! 3. `committed_and_uncommitted_peer_files_both` — one of each: the phrase is
//!    preceded by exactly `"1 committed file(s) and 1 uncommitted file(s)"`, and
//!    the note's file list is exactly `{peer/committed.rs, peer/uncommitted.rs}`.
//!
//! Fixture observation (scenario 2): the gate reads `git status --porcelain=v1 -z`
//! (`crates/tdd/src/git.rs`), which without `-uall` collapses an untracked
//! file inside an otherwise-untracked directory to the directory entry
//! `peer/`. So scenario 2 commits `peer/.gitkeep` before the session, making
//! `peer/` tracked and the peer file listed by its own path. This is a fixture
//! choice to make the file name observable; it is not an endorsement of the
//! directory-collapse behaviour, which these tests do not examine.
//!
//! Measurement point: e72e9bd37, 2026-11-16.
//!
//! What it does NOT prove: that attribution (which files are excluded) is
//! right in general — only in these three fixtures; counts > 1, the 12-file
//! truncation `(+N more)`, renamed/deleted files, merge commits; a file that
//! is BOTH committed in the window and changed again in the working tree
//! (classified "uncommitted" by `mark_committed`; not exercised here); that
//! the block-reason site is reached at all (the phrase check applies to every
//! occurrence found, and at least one — the stderr note — is required).
//!
//! Written by an independent test author, not the implementer.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SESSION_START: &str = "2026-06-01T05:00:00.000Z";
const IN_SESSION_WRITE: &str = "2026-06-01T05:10:00.000Z";
const PEER_COMMIT: &str = "2026-06-01T05:15:00Z";
const IN_SESSION_COMMIT: &str = "2026-06-01T05:20:00Z";
const SESSION_LAST: &str = "2026-06-01T05:30:00.000Z";

/// The fixed middle of both message sites.
const PHRASE: &str = ", not attributed to this session, were excluded";
/// The stderr note's prefix and the separator before its file list.
const NOTE_PREFIX: &str = "tdd: ";
const NOTE_FROM_SCAN: &str = ", not attributed to this session, were excluded from the scan";

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
        "git {args:?} failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
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

/// A fresh trusted repo with one pre-session commit. Returns (home, repo).
fn setup(tag: &str) -> (PathBuf, PathBuf) {
    let home = std::env::temp_dir().join(format!("tdd-excl-label-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let repo = home.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"], &[]);
    git(&repo, &["config", "user.email", "t@t.com"], &[]);
    git(&repo, &["config", "user.name", "t"], &[]);
    git(&repo, &["config", "commit.gpgsign", "false"], &[]);
    std::fs::write(repo.join("seed.txt"), "seed\n").unwrap();
    commit_at(&repo, "init", "2026-01-01T00:00:00Z");
    let repo = repo.canonicalize().unwrap();
    std::fs::create_dir_all(home.join(".harness")).unwrap();
    std::fs::write(
        home.join(".harness/trust.toml"),
        format!("trusted = [\"{}\"]\n", repo.display()),
    )
    .unwrap();
    (home, repo)
}

/// This session's own impl + test, written via Write in the transcript and
/// committed inside the session window. Returns (lib, test) paths.
fn session_work(repo: &Path) -> (PathBuf, PathBuf) {
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(repo.join("tests")).unwrap();
    let lib = repo.join("src/lib.rs");
    let t = repo.join("tests/t.rs");
    std::fs::write(&lib, "pub fn f() -> i32 {\n    let x = 1;\n    x + 1\n}\n").unwrap();
    std::fs::write(&t, "#[test]\nfn f_works() {\n    assert_eq!(2, 2);\n}\n").unwrap();
    commit_at(repo, "session work", IN_SESSION_COMMIT);
    (lib, t)
}

/// Write the transcript (Writes of `lib` and `t` only) and run `tdd gate`.
/// Returns (stdout, stderr).
fn run_gate(home: &Path, repo: &Path, sid: &str, lib: &Path, t: &Path) -> (String, String) {
    let write = |p: &Path, id: &str| {
        serde_json::json!({
            "type": "assistant", "parentUuid": "u0", "isSidechain": false,
            "sessionId": sid,
            "message": {"model": "claude-opus-5", "id": id, "type": "message",
                "role": "assistant",
                "content": [{"type": "tool_use", "id": id, "name": "Write",
                    "input": {"file_path": p.to_string_lossy(), "content": "x"}}]}
        })
    };
    let recs = [
        rec(
            serde_json::json!({"type": "user", "uuid": "u0", "sessionId": sid,
                "message": {"role": "user", "content": "go"}}),
            SESSION_START,
        ),
        rec(write(lib, "m1"), IN_SESSION_WRITE),
        rec(write(t, "m2"), IN_SESSION_WRITE),
        rec(
            serde_json::json!({"type": "assistant", "parentUuid": "m2", "isSidechain": false,
                "sessionId": sid,
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
        "session_id": sid,
        "transcript_path": transcript.to_string_lossy(),
        "cwd": repo.to_string_lossy(),
        "hook_event_name": "Stop",
        "stop_hook_active": false,
    })
    .to_string();
    let mut child = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_tdd")))
        .arg("gate")
        .current_dir(repo)
        .env("HOME", home)
        .env("CLAUDE_CODE_SESSION_ID", sid)
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
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Every occurrence of [`PHRASE`] in `all` must be immediately preceded by
/// exactly `expected` counts, which itself must start right after `"tdd: "`
/// (the note) or `". "` (the block reason) — so a longer counts string that
/// merely ENDS with `expected` (e.g. "1 committed file(s) and 1 uncommitted
/// file(s)" vs "1 uncommitted file(s)") is rejected. At least one occurrence
/// is required.
fn assert_counts_everywhere(all: &str, expected: &str) {
    let mut n = 0;
    for (i, _) in all.match_indices(PHRASE) {
        n += 1;
        let before = &all[..i];
        let ok = before.ends_with(&format!("{NOTE_PREFIX}{expected}"))
            || before.ends_with(&format!(". {expected}"));
        let ctx_start = before.len().saturating_sub(80);
        assert!(
            ok,
            "occurrence #{n} of the excluded phrase is not preceded by exactly {expected:?}: \
             ...{:?}",
            &all[ctx_start..(i + PHRASE.len()).min(all.len())]
        );
    }
    assert!(
        n >= 1,
        "the excluded-files phrase never appeared in:\n{all}"
    );
}

/// The stderr note lines (`tdd: <counts>, not attributed ... from the scan (...): <files>`),
/// each parsed to (counts, file set).
fn note_lines(stderr: &str) -> Vec<(String, BTreeSet<String>)> {
    stderr
        .lines()
        .filter_map(|l| {
            let rest = l.strip_prefix(NOTE_PREFIX)?;
            let (counts, tail) = rest.split_once(NOTE_FROM_SCAN)?;
            let (_, files) = tail.rsplit_once("): ")?;
            Some((
                counts.to_string(),
                files.split(", ").map(str::to_string).collect(),
            ))
        })
        .collect()
}

/// Exactly the expected (counts, files) in every note line, and at least one.
fn assert_note(stderr: &str, counts: &str, files: &[&str]) {
    let notes = note_lines(stderr);
    assert!(
        !notes.is_empty(),
        "no attribution note on stderr:\n{stderr}"
    );
    let want: BTreeSet<String> = files.iter().map(|s| s.to_string()).collect();
    for (c, f) in &notes {
        assert_eq!(c, counts, "note counts differ\nstderr:\n{stderr}");
        assert_eq!(f, &want, "note file list differs\nstderr:\n{stderr}");
    }
}

/// Does `s` contain `"committed file(s)"` NOT immediately preceded by `"un"`?
fn has_bare_committed_count(s: &str) -> bool {
    s.match_indices("committed file(s)")
        .any(|(i, _)| !s[..i].ends_with("un"))
}

#[test]
fn committed_peer_file_only() {
    let (home, repo) = setup("committed");
    // A peer's non-merge commit inside the session window; never in the transcript.
    std::fs::create_dir_all(repo.join("peer")).unwrap();
    std::fs::write(
        repo.join("peer/other.rs"),
        "pub fn peer() -> u8 {\n    7\n}\n",
    )
    .unwrap();
    commit_at(&repo, "peer work", PEER_COMMIT);
    let (lib, t) = session_work(&repo);
    // Control: nothing uncommitted, so the peer file is known only from history.
    assert_eq!(
        git(&repo, &["status", "--porcelain"], &[]),
        "",
        "control: clean tree"
    );

    let (stdout, stderr) = run_gate(&home, &repo, "s-committed-only", &lib, &t);
    let _ = std::fs::remove_dir_all(&home);
    let all = format!("{stdout}\n{stderr}");

    assert_counts_everywhere(&all, "1 committed file(s)");
    assert_note(&stderr, "1 committed file(s)", &["peer/other.rs"]);
    assert!(
        !all.contains("uncommitted file(s)"),
        "an uncommitted count appeared for a committed-only peer file:\n{all}"
    );
}

#[test]
fn uncommitted_peer_file_only() {
    let (home, repo) = setup("uncommitted");
    // See the module doc: makes `peer/` tracked so porcelain lists the file itself.
    std::fs::create_dir_all(repo.join("peer")).unwrap();
    std::fs::write(repo.join("peer/.gitkeep"), "").unwrap();
    commit_at(&repo, "peer dir", "2026-01-01T00:30:00Z");
    let (lib, t) = session_work(&repo);
    // A peer's untracked file, never named in the transcript.
    std::fs::write(
        repo.join("peer/peer_uncommitted.rs"),
        "pub fn other_peer() -> u8 {\n    42\n}\n",
    )
    .unwrap();
    // Control: porcelain names the file itself, not the collapsed directory.
    assert_eq!(
        git(&repo, &["status", "--porcelain"], &[]),
        "?? peer/peer_uncommitted.rs\n",
        "control: the peer file is listed by its own path"
    );

    let (stdout, stderr) = run_gate(&home, &repo, "s-uncommitted-only", &lib, &t);
    let _ = std::fs::remove_dir_all(&home);
    let all = format!("{stdout}\n{stderr}");

    assert_counts_everywhere(&all, "1 uncommitted file(s)");
    assert_note(
        &stderr,
        "1 uncommitted file(s)",
        &["peer/peer_uncommitted.rs"],
    );
    assert!(
        !has_bare_committed_count(&all),
        "a committed count (not 'uncommitted') appeared for an uncommitted-only peer file:\n{all}"
    );
}

#[test]
fn committed_and_uncommitted_peer_files_both() {
    let (home, repo) = setup("both");
    // A peer's commit in the session window.
    std::fs::create_dir_all(repo.join("peer")).unwrap();
    std::fs::write(
        repo.join("peer/committed.rs"),
        "pub fn committed() -> u8 {\n    1\n}\n",
    )
    .unwrap();
    commit_at(&repo, "peer committed work", PEER_COMMIT);
    let (lib, t) = session_work(&repo);
    // A peer's untracked file (`peer/` is tracked via peer/committed.rs).
    std::fs::write(
        repo.join("peer/uncommitted.rs"),
        "pub fn uncommitted() -> u8 {\n    2\n}\n",
    )
    .unwrap();
    assert_eq!(
        git(&repo, &["status", "--porcelain"], &[]),
        "?? peer/uncommitted.rs\n",
        "control: only the uncommitted peer file is in the working tree"
    );

    let (stdout, stderr) = run_gate(&home, &repo, "s-both", &lib, &t);
    let _ = std::fs::remove_dir_all(&home);
    let all = format!("{stdout}\n{stderr}");

    let counts = "1 committed file(s) and 1 uncommitted file(s)";
    assert_counts_everywhere(&all, counts);
    assert_note(
        &stderr,
        counts,
        &["peer/committed.rs", "peer/uncommitted.rs"],
    );
}
