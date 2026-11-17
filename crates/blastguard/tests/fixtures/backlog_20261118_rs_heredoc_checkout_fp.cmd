cd /Users/yuki/src/.harness-worktrees/red-unnamed-write-attr && cat > crates/tdd/tests/backlog_20261118_unnamed_write_excluded_from_scope.rs <<'EOF'
// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! REPRO (suspected fail-open, 2026-11-18): the `tdd` Stop gate narrows its
//! scope to "files this session wrote" via `scope_to_session` →
//! `harness_core::attribution::attribute_own_writes`. That function keeps a
//! changed file iff an Edit/Write names it or a Bash statement names its path
//! in a non-read-only stage; everything else is "attributed to nobody and is
//! EXCLUDED" (attribution.rs doc).
//!
//! A session that WROTE files without naming them (`cargo fmt`, a glob
//! `sed -i 's/a/b/' src/*.rs`, a code generator, `git stash pop`,
//! `git checkout <branch> -- .`) therefore gets those files dropped from the
//! gate's scope: the gate never inspects 14 new implementation lines that no
//! test accompanies. CLAUDE.md section 3: an unattributable write is
//! "cannot determine" and must resolve to the restrictive side (keep in
//! scope), not to exclusion.
//!
//! NOTE for the reader: the 2026-10-17 user ruling (see reviewgate's
//! `session_attribution.rs` header) explicitly "accepted" this residual hole.
//! These tests pin the CLAUDE.md section 3 reading against that acceptance;
//! if the ruling stands, close the RED as won't-do rather than weakening it.
//!
//! Tests:
//! * `control_edit_naming_the_file_blocks` - the fixture can block (passes).
//! * `control_read_only_session_still_excludes_foreign_change` - a session
//!   that only ran `git status` did not write the file, so exclusion is right
//!   (passes today and must keep passing after a fix).
//! * `unnamed_write_cargo_fmt_stays_in_scope` / `unnamed_write_glob_sed_stays_in_scope`
//!   - RED: the transcript has a path-less write; the gate must still judge
//!   the changed implementation file.
//!
//! Written by a disinterested author (CLAUDE.md 2(a)).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

const SESSION: &str = "s-unnamed-write";
const IMPL_REL: &str = "crates/x/src/lib.rs";
const SEED_DATE: &str = "2026-01-01T00:00:00Z";
const SESSION_START: &str = "2026-06-01T05:00:00.000Z";
const SESSION_LAST: &str = "2026-06-01T05:40:00.000Z";

/// 14 implementation lines, no test marker.
fn impl_body() -> String {
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

impl Fixture {
    /// A repo with a committed (empty) `IMPL_REL`, then an UNCOMMITTED working
    /// tree change adding 14 implementation lines and no test.
    fn new() -> Fixture {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!("tdd-unnamed-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let repo = home.join("project");
        std::fs::create_dir_all(repo.join("crates/x/src")).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t.com"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join(IMPL_REL), "").unwrap();
        git(&repo, &["add", "-A"]);
        git_env(
            &repo,
            &["commit", "-qm", "seed"],
            &[
                ("GIT_AUTHOR_DATE", SEED_DATE),
                ("GIT_COMMITTER_DATE", SEED_DATE),
            ],
        );
        std::fs::write(repo.join(IMPL_REL), impl_body()).unwrap();

        let repo = repo.canonicalize().unwrap();
        std::fs::create_dir_all(home.join(".harness")).unwrap();
        std::fs::write(
            home.join(".harness/trust.toml"),
            format!("trusted = [\"{}\"]\n", repo.display()),
        )
        .unwrap();
        Fixture { home, repo }
    }

    fn transcript(&self, uses: &[serde_json::Value]) -> PathBuf {
        let d = self.home.join(".claude/projects/-home-t-project");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join(format!("{SESSION}.jsonl"));
        std::fs::write(&p, records(uses)).unwrap();
        p
    }

    /// Run the real `tdd gate` Stop hook. Returns `(blocked, stdout, stderr)`.
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

fn records(uses: &[serde_json::Value]) -> String {
    let mut body = serde_json::json!({
        "type": "user", "timestamp": SESSION_START, "sessionId": SESSION,
        "message": {"role": "user", "content": "go"}
    })
    .to_string();
    body.push('\n');
    for (i, u) in uses.iter().enumerate() {
        let mut rec = u.clone();
        rec["isSidechain"] = serde_json::json!(false);
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

fn bash(cmd: &str, cwd: &Path) -> serde_json::Value {
    serde_json::json!({
        "type": "assistant", "cwd": cwd.to_string_lossy(),
        "message": {"model": "claude-opus-5", "id": "m", "type": "message", "role": "assistant",
            "content": [{"type": "tool_use", "id": "t", "name": "Bash",
                "input": {"command": cmd, "description": "run"}}]}
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

/// CONTROL (passes today): the session Edited the file, so its 14 untested
/// implementation lines are in scope and the gate blocks.
#[test]
fn control_edit_naming_the_file_blocks() {
    let f = Fixture::new();
    let t = f.transcript(&[edit(&f.repo.join(IMPL_REL))]);
    let (blocked, out, err) = f.stop(&t);
    assert!(
        blocked,
        "control: an attributed file with 14 untested impl lines must block\nstdout: {out}\nstderr: {err}"
    );
}

/// CONTROL (passes today, must keep passing after a fix): a read-only session
/// (`git status`) wrote nothing, so a foreign change is rightly excluded.
#[test]
fn control_read_only_session_still_excludes_foreign_change() {
    let f = Fixture::new();
    let t = f.transcript(&[bash("git status", &f.repo)]);
    let (blocked, out, err) = f.stop(&t);
    assert!(
        !blocked,
        "a read-only session must not own a foreign change\nstdout: {out}\nstderr: {err}"
    );
}

/// RED: the session ran `cargo fmt`, which rewrites files without naming any.
/// A path-less write is unattributable -> must stay in scope (restrictive
/// side), so the untested implementation lines must still be judged.
// test-weakening-justified: new RED for an unfixed defect (2026-11-18).
#[test]
#[ignore = "RED: 2026-11-18 path-less Bash write (cargo fmt) excludes changed files from tdd scope (fail-open)"]
fn unnamed_write_cargo_fmt_stays_in_scope() {
    let f = Fixture::new();
    let t = f.transcript(&[bash("cargo fmt", &f.repo)]);
    let (blocked, out, err) = f.stop(&t);
    assert!(
        blocked,
        "a session that ran `cargo fmt` (path-less write) must keep the changed impl file in scope\nstdout: {out}\nstderr: {err}"
    );
}

/// RED: a glob `sed -i` rewrites files without naming their paths.
// test-weakening-justified: new RED for an unfixed defect (2026-11-18).
#[test]
#[ignore = "RED: 2026-11-18 path-less Bash write (glob sed -i) excludes changed files from tdd scope (fail-open)"]
fn unnamed_write_glob_sed_stays_in_scope() {
    let f = Fixture::new();
    let t = f.transcript(&[bash("sed -i 's/a/b/' crates/x/src/*.rs", &f.repo)]);
    let (blocked, out, err) = f.stop(&t);
    assert!(
        blocked,
        "a session that ran a glob `sed -i` must keep the changed impl file in scope\nstdout: {out}\nstderr: {err}"
    );
}
EOF
. "$HOME/.cargo/env"; cargo test -p tdd --test backlog_20261118_unnamed_write_excluded_from_scope -- --include-ignored 2>&1 | tail -40