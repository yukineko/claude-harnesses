#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `mode = "loose"` in tdd.toml: warn once per (path, content hash), across
//! sessions. `strict` (default / unknown value) keeps blocking.
//! Written by a disinterested author (CLAUDE.md section 2(a)).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);
const IMPL_BODY: &str = "fn f() {\n    let x = 1;\n    println!(\"{}\", x);\n}\n";

struct Fx {
    home: PathBuf,
    repo: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let s = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(s.success(), "git {args:?}");
}

impl Fx {
    fn new(mode_line: Option<&str>) -> Fx {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!("tdd-loose-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let repo = home.join("project");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "t@t.com"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("seed.rs"), "fn seed() {}\n").unwrap();
        git(&repo, &["add", "seed.rs"]);
        git(
            &repo,
            &["-c", "commit.gpgsign=false", "commit", "-qm", "init"],
        );
        let repo = repo.canonicalize().unwrap_or(repo);
        let state = home.join("tdd-state");
        let mut toml = format!("state_dir = \"{}\"\n", state.display());
        if let Some(m) = mode_line {
            toml.push_str(m);
            toml.push('\n');
        }
        std::fs::write(repo.join("tdd.toml"), toml).unwrap();
        Fx { home, repo }
    }

    fn dirty(&self, name: &str) {
        std::fs::write(self.repo.join(name), IMPL_BODY).unwrap();
    }

    fn append(&self, name: &str) {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(self.repo.join(name))
            .unwrap();
        f.write_all(b"fn extra() { let y = 2; println!(\"{}\", y); }\n")
            .unwrap();
    }

    /// Returns stdout JSON (Null when empty).
    fn stop(&self, session: &str) -> serde_json::Value {
        let payload = serde_json::json!({
            "session_id": session,
            "transcript_path": self.home.join("no-such-transcript.jsonl").to_string_lossy(),
            "cwd": self.repo.to_string_lossy(),
            "hook_event_name": "Stop",
            "stop_hook_active": false,
        })
        .to_string();
        let mut child = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_tdd")))
            .arg("gate")
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_SESSION_ID", session)
            .env("HARNESS_TRUST_ALL", "1")
            .env_remove("TDD_DISABLE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(0));
        let s = String::from_utf8_lossy(&out.stdout).into_owned();
        if s.trim().is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_str(s.trim()).unwrap_or_else(|_| panic!("bad stdout: {s}"))
        }
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn blocked(v: &serde_json::Value) -> bool {
    v.get("decision").and_then(|d| d.as_str()) == Some("block")
}

fn silent(v: &serde_json::Value) -> bool {
    v.get("decision").is_none() && v.get("systemMessage").is_none()
}

#[test]
fn default_mode_is_strict_across_sessions() {
    let f = Fx::new(None);
    f.dirty("a.rs");
    assert!(blocked(&f.stop("s1")));
    assert!(blocked(&f.stop("s2")));
}

#[test]
fn loose_same_session_second_stop_is_silent() {
    let f = Fx::new(Some("mode = \"loose\""));
    f.dirty("a.rs");
    assert!(blocked(&f.stop("s1")));
    let v = f.stop("s1");
    assert!(silent(&v), "expected silence, got {v}");
}

#[test]
fn loose_across_sessions_is_silent() {
    let f = Fx::new(Some("mode = \"loose\""));
    f.dirty("a.rs");
    assert!(blocked(&f.stop("sA")));
    let v = f.stop("sB");
    assert!(silent(&v), "expected silence, got {v}");
}

#[test]
fn loose_edit_flagged_file_blocks_again() {
    let f = Fx::new(Some("mode = \"loose\""));
    f.dirty("a.rs");
    assert!(blocked(&f.stop("s1")));
    assert!(silent(&f.stop("s1")));
    f.append("a.rs");
    assert!(blocked(&f.stop("s1")));
}

#[test]
fn loose_new_impl_file_blocks_again() {
    let f = Fx::new(Some("mode = \"loose\""));
    f.dirty("a.rs");
    assert!(blocked(&f.stop("s1")));
    assert!(silent(&f.stop("s1")));
    f.dirty("b.rs");
    assert!(blocked(&f.stop("s1")));
}

#[test]
fn bogus_mode_behaves_strict() {
    let f = Fx::new(Some("mode = \"bogus\""));
    f.dirty("a.rs");
    assert!(blocked(&f.stop("s1")));
    assert!(blocked(&f.stop("s2")));
}
