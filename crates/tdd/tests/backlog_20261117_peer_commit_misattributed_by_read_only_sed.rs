// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! REPRO (observed 2026-11-17, session 1e000416): the `tdd` Stop gate blocked
//! a session with a clean main working tree on "14 new implementation line(s)
//! added … implementation changed: crates/blastguard/src/detect/ast/judge.rs".
//! The 14 lines were ANOTHER session's commit 772851b76 (merged into main by
//! d008fb18c after this session started). Session 1e000416 never wrote
//! judge.rs.
//!
//! Path traced in the code (not assumed):
//!
//! 1. `tdd::gate::include_committed` folds the added lines of every non-merge
//!    commit on HEAD dated at/after the session start — the peer's commit
//!    included — and relies on `scope_to_session` to drop files this session
//!    did not write ("a peer's commit in the same window is excluded by the
//!    same rule").
//! 2. `scope_to_session` → `harness_core::attribution::attribute_own_writes`
//!    keeps a file iff an Edit/Write names it or a Bash command names its path
//!    in a statement `writing_statements` does not drop as read-only.
//! 3. `read_only_stage` (harness-core/src/attribution.rs) has no arm for
//!    `sed`, so the print-only `sed -n 40,212p <path>` the session's subagent
//!    ran (sidechain agent-aafd4626885243b75, 2026-11-16T22:35:49Z, before the
//!    first block at 22:40:17Z) counts as a WRITE of judge.rs. judge.rs is
//!    therefore attributed to the session and the peer's committed lines are
//!    judged as the session's.
//!
//! The parent's `git -C … log … -- judge.rs … | head` (the other hypothesis)
//! came AFTER the first block, and `git log` / `head` are read-only stages, so
//! it is not the cause; [`git_log_naming_the_path_does_not_attribute_a_peer_commit`]
//! pins that as an observation rather than a judgement.
//!
//! [`control_subagent_edit_of_the_file_does_block`] shows the fixture CAN
//! block (the peer commit is folded and judged once the file is attributed),
//! so the allow-side assertions are not vacuous.
//!
//! Written by a disinterested author (CLAUDE.md 2(a)): the gate's
//! implementer did not write this file.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

const SESSION: &str = "s-misattr";
const IMPL_REL: &str = "crates/x/src/detect/ast/judge.rs";
const SEED_DATE: &str = "2026-01-01T00:00:00Z";
const SESSION_START: &str = "2026-06-01T05:00:00.000Z";
const PEER_COMMIT: &str = "2026-06-01T05:20:00Z";
const SESSION_LAST: &str = "2026-06-01T05:40:00.000Z";

/// 14 implementation lines, no test marker — the shape of 772851b76's judge.rs.
fn peer_impl() -> String {
    let mut s = String::from("pub enum Effect {\n");
    for i in 0..12 {
        s.push_str(&format!("    Variant{i},\n"));
    }
    s.push_str("}\n");
    s
}

struct Fixture {
    home: PathBuf,
    repo: PathBuf,
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

fn git(dir: &Path, args: &[&str]) {
    git_env(dir, args, &[]);
}

fn dated(date: &str) -> [(&'static str, String); 2] {
    [
        ("GIT_AUTHOR_DATE", date.to_string()),
        ("GIT_COMMITTER_DATE", date.to_string()),
    ]
}

fn git_at(dir: &Path, args: &[&str], date: &str) {
    let env = dated(date);
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    git_env(dir, args, &env);
}

impl Fixture {
    /// A repo whose HEAD carries a PEER's non-merge commit (dated inside the
    /// session window) adding 14 impl lines to `IMPL_REL`, brought in by a
    /// `--no-ff` merge (the 772851b76 / d008fb18c topology). Working tree
    /// clean.
    fn new() -> Fixture {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!("tdd-misattr-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let repo = home.join("project");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t.com"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::create_dir_all(repo.join("crates/x/src/detect/ast")).unwrap();
        std::fs::write(
            repo.join("crates/x/src/detect/ast/mod.rs"),
            "pub mod judge;\n",
        )
        .unwrap();
        std::fs::write(repo.join(IMPL_REL), "").unwrap();
        git(&repo, &["add", "-A"]);
        git_at(&repo, &["commit", "-qm", "seed"], SEED_DATE);

        // The peer's branch and commit.
        git(&repo, &["checkout", "-q", "-b", "peer"]);
        std::fs::write(repo.join(IMPL_REL), peer_impl()).unwrap();
        git(&repo, &["add", "-A"]);
        git_at(
            &repo,
            &["commit", "-qm", "peer: S1 subset spec"],
            PEER_COMMIT,
        );
        git(&repo, &["checkout", "-q", "main"]);
        git_at(
            &repo,
            &["merge", "-q", "--no-ff", "-m", "Merge peer", "peer"],
            PEER_COMMIT,
        );

        let repo = repo.canonicalize().unwrap();
        std::fs::create_dir_all(home.join(".harness")).unwrap();
        std::fs::write(
            home.join(".harness/trust.toml"),
            format!("trusted = [\"{}\"]\n", repo.display()),
        )
        .unwrap();
        Fixture { home, repo }
    }

    fn slug_dir(&self) -> PathBuf {
        let d = self.home.join(".claude/projects/-home-t-project");
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Parent transcript: a user prompt at `SESSION_START`, then `uses`, then a
    /// closing record at `SESSION_LAST`.
    fn transcript(&self, uses: &[serde_json::Value]) -> PathBuf {
        let p = self.slug_dir().join(format!("{SESSION}.jsonl"));
        std::fs::write(&p, records(uses, false)).unwrap();
        p
    }

    /// The subagent sidechain `<slug>/<session>/subagents/agent-1.jsonl`.
    fn sidechain(&self, uses: &[serde_json::Value]) {
        let d = self.slug_dir().join(SESSION).join("subagents");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("agent-1.jsonl"), records(uses, true)).unwrap();
    }

    /// Run the real `tdd gate` Stop hook with cwd = the repo. Returns
    /// `(blocked, stdout, stderr)`.
    fn stop(&self, transcript: &Path) -> (bool, String, String) {
        let payload = serde_json::json!({
            "session_id": SESSION,
            "transcript_path": transcript.to_string_lossy(),
            "cwd": self.repo.to_string_lossy(),
            "hook_event_name": "Stop",
            "stop_hook_active": false,
        })
        .to_string();
        let mut child = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_tdd")))
            .arg("gate")
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_SESSION_ID", SESSION)
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
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let blocked = serde_json::from_str::<serde_json::Value>(stdout.trim())
            .ok()
            .and_then(|v| v.get("decision").and_then(|d| d.as_str()).map(String::from))
            .as_deref()
            == Some("block");
        (
            blocked,
            stdout,
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn records(uses: &[serde_json::Value], sidechain: bool) -> String {
    let mut body = serde_json::json!({
        "type": "user", "timestamp": SESSION_START, "sessionId": SESSION,
        "message": {"role": "user", "content": "go"}
    })
    .to_string();
    body.push('\n');
    for (i, u) in uses.iter().enumerate() {
        let mut rec = u.clone();
        rec["isSidechain"] = serde_json::json!(sidechain);
        rec["sessionId"] = serde_json::json!(SESSION);
        rec["timestamp"] = serde_json::json!(format!("2026-06-01T05:3{i}:00.000Z"));
        body.push_str(&rec.to_string());
        body.push('\n');
    }
    body.push_str(
        &serde_json::json!({
            "type": "assistant", "timestamp": SESSION_LAST, "sessionId": SESSION,
            "message": {"role": "assistant", "content": [{"type": "text", "text": "done"}]}
        })
        .to_string(),
    );
    body.push('\n');
    body
}

/// A `Bash` tool_use record whose transcript `cwd` is `cwd`.
fn bash(cmd: &str, cwd: &Path) -> serde_json::Value {
    serde_json::json!({
        "type": "assistant", "cwd": cwd.to_string_lossy(),
        "message": {"model": "claude-opus-5", "id": "m", "type": "message", "role": "assistant",
            "content": [{"type": "tool_use", "id": "t", "name": "Bash",
                "input": {"command": cmd, "description": "read"}}]}
    })
}

fn edit(path: &Path) -> serde_json::Value {
    serde_json::json!({
        "type": "assistant",
        "message": {"model": "claude-opus-5", "id": "m", "type": "message", "role": "assistant",
            "content": [{"type": "tool_use", "id": "t", "name": "Edit",
                "input": {"file_path": path.to_string_lossy(), "old_string": "a", "new_string": "b"}}]}
    })
}

/// CONTROL: a subagent that really EDITED the file makes the peer's committed
/// lines count (file-granular attribution) — so the fixture can block, and the
/// allow assertions below observe the attribution, not a dead fixture.
#[test]
fn control_subagent_edit_of_the_file_does_block() {
    let f = Fixture::new();
    f.sidechain(&[edit(&f.repo.join(IMPL_REL))]);
    let t = f.transcript(&[]);
    let (blocked, out, err) = f.stop(&t);
    assert!(
        blocked,
        "control: an attributed file with 14 committed impl lines must block\nstdout: {out}\nstderr: {err}"
    );
}

/// CONTROL: nothing names the file → the peer's commit is excluded and the
/// stop is allowed (the intended `include_committed` + `scope_to_session` rule).
#[test]
fn peer_commit_on_unnamed_file_is_not_attributed() {
    let f = Fixture::new();
    let t = f.transcript(&[]);
    let (blocked, out, err) = f.stop(&t);
    assert!(
        !blocked,
        "a peer's post-start commit on a file this session never named must not block\nstdout: {out}\nstderr: {err}"
    );
}

/// The parent session's actual command (line 133 of the 1e000416 transcript,
/// paths re-rooted to the fixture). `git log` and `head` are read-only stages,
/// so naming the path here must not attribute the peer's commit.
#[test]
fn git_log_naming_the_path_does_not_attribute_a_peer_commit() {
    let f = Fixture::new();
    let cmd = format!(
        "git -C {wt} status --short; git -C {repo} log --since='3 hours ago' --format='%h %an %s' -- {IMPL_REL} .backlog/tasks.toml | head",
        wt = f.home.join("session-wt").display(),
        repo = f.repo.display(),
    );
    let t = f.transcript(&[bash(&cmd, &f.repo)]);
    let (blocked, out, err) = f.stop(&t);
    assert!(
        !blocked,
        "a read-only `git log -- <path>` must not make a peer's committed lines this session's\nstdout: {out}\nstderr: {err}"
    );
}

/// The subagent's actual command (agent-aafd4626885243b75 line 67, paths
/// re-rooted). `sed -n …p` only PRINTS; it does not write the file. The peer's
/// 14 committed lines in that file are not this session's, so the stop must be
/// allowed. (Was RED until `read_only_stage` learned a provably print-only
/// `sed`.)
#[test]
fn print_only_sed_naming_the_path_does_not_attribute_a_peer_commit() {
    let f = Fixture::new();
    let wt = f.home.join("session-wt");
    let cmd = format!(
        "cd {wt} && sed -n 1,80p crates/x/src/detect/ast/mod.rs; sed -n 315,420p crates/x/src/detect/ast/mod.rs; sed -n 40,212p {IMPL_REL}",
        wt = wt.display(),
    );
    f.sidechain(&[bash(&cmd, &f.repo)]);
    let t = f.transcript(&[]);
    let (blocked, out, err) = f.stop(&t);
    assert!(
        !blocked,
        "a print-only `sed -n …p <path>` must not make a peer's committed lines this session's\nstdout: {out}\nstderr: {err}"
    );
}
