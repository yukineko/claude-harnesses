// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! SPEC CHANGE (user ruling 2026-10-27), superseding the earlier ruling
//! "allow but never silently" that this file used to pin (it asserted a visible
//! `systemMessage` "donegate not configured for <root>: no checks ran").
//! The user chose to REMOVE that systemMessage. For a project with NO donegate
//! config at all (Declaration::Absent) whose session wrote files, the Stop hook
//! still ALLOWS (unchanged), but stdout is EMPTY (no `systemMessage`, no
//! `decision`). A durable trace replaces the UI message: one JSONL line appended
//! to `<state_dir>/log.jsonl` (state_dir defaults to `$HOME/.donegate/state`)
//! with `"verdict":"unconfigured"`, the session id, and each unconfigured root.
//! Contrast (control, unchanged): a config that EXISTS but yields an empty check
//! set for a root the session wrote into still BLOCKS.
//!
//! Fixture style mirrors `session_worktree_roots.rs`: `$HOME` is a tempdir whose
//! `.harness/trust.toml` lists the canonical trees, `HARNESS_TRUST_ALL` removed,
//! session id pinned, transcript JSONL with an Edit tool_use by absolute path.
//!
//! Written by a disinterested author (CLAUDE.md section 2(a)).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

struct Fixture {
    home: PathBuf,
    primary: PathBuf,
    wt: PathBuf,
}

struct Outcome {
    stdout: String,
    stderr: String,
    json: Option<serde_json::Value>,
}

impl Outcome {
    fn field(&self, k: &str) -> String {
        self.json
            .as_ref()
            .and_then(|v| v.get(k))
            .and_then(|d| d.as_str())
            .unwrap_or("")
            .to_string()
    }
}

fn git(dir: &Path, args: &[&str]) {
    let st = harness_core::git_env::scrub(&mut Command::new("git"))
        .current_dir(dir)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git runs");
    assert!(st.success(), "git {args:?} failed in {}", dir.display());
}

impl Fixture {
    /// `toml`: `Some(text)` commits a donegate.toml in the primary tree;
    /// `None` leaves the whole project without any donegate config.
    fn new(toml: Option<&str>) -> Fixture {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!("dg-nocfg-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let primary = home.join("primary");
        std::fs::create_dir_all(&primary).unwrap();
        git(&primary, &["init", "-q"]);
        git(&primary, &["config", "user.email", "t@t.com"]);
        git(&primary, &["config", "user.name", "t"]);
        std::fs::write(primary.join("seed.rs"), "fn seed() {}\n").unwrap();
        git(&primary, &["add", "seed.rs"]);
        if let Some(t) = toml {
            std::fs::write(primary.join("donegate.toml"), t).unwrap();
            git(&primary, &["add", "donegate.toml"]);
        }
        git(
            &primary,
            &["-c", "commit.gpgsign=false", "commit", "-qm", "init"],
        );
        let wt = home.join("linked-wt");
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "wt-branch",
                wt.to_str().unwrap(),
            ],
        );
        // Uncommitted session edits in both trees.
        std::fs::write(primary.join("session_edit.rs"), "fn edited() {}\n").unwrap();
        std::fs::write(wt.join("session_edit.rs"), "fn edited() {}\n").unwrap();

        let primary = primary.canonicalize().unwrap();
        let wt = wt.canonicalize().unwrap();
        std::fs::create_dir_all(home.join(".harness")).unwrap();
        std::fs::write(
            home.join(".harness/trust.toml"),
            format!(
                "trusted = [\"{}\", \"{}\"]\n",
                primary.display(),
                wt.display()
            ),
        )
        .unwrap();
        Fixture { home, primary, wt }
    }

    fn transcript(&self, file: &Path) -> PathBuf {
        let d = self.home.join(".claude/projects/-slug");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("sess.jsonl");
        let mut body = String::from(r#"{"type":"user","message":{"role":"user","content":"go"}}"#);
        body.push('\n');
        body.push_str(
            &serde_json::json!({
                "parentUuid": "p", "isSidechain": false,
                "message": {"model": "claude-opus-5", "id": "m1", "type": "message",
                    "role": "assistant",
                    "content": [{"type": "tool_use", "id": "t1", "name": "Edit",
                        "input": {"file_path": file.to_string_lossy(),
                                  "old_string": "a", "new_string": "b"}}]}
            })
            .to_string(),
        );
        body.push('\n');
        std::fs::write(&p, body).unwrap();
        p
    }

    fn stop(&self, cwd: &Path, transcript: &Path) -> Outcome {
        let payload = serde_json::json!({
            "session_id": "s-nocfg",
            "transcript_path": transcript.to_string_lossy(),
            "cwd": cwd.to_string_lossy(),
            "hook_event_name": "Stop",
            "stop_hook_active": false,
        })
        .to_string();
        let mut child =
            harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_donegate")))
                .arg("gate")
                .current_dir(cwd)
                .env("HOME", &self.home)
                .env("CLAUDE_CODE_SESSION_ID", "s-nocfg")
                .env_remove("DONEGATE_DISABLE")
                .env_remove("HARNESS_TRUST_ALL")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("donegate spawns");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(0), "Stop hook must exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let json = serde_json::from_str::<serde_json::Value>(stdout.trim())
            .ok()
            .or_else(|| {
                stdout
                    .lines()
                    .rev()
                    .find_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
            });
        Outcome {
            stdout,
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            json,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn assert_quiet_allow_with_trace(f: &Fixture, o: &Outcome, root: &Path) {
    assert_ne!(
        o.field("decision"),
        "block",
        "no config at all must allow, not block; stdout={:?} stderr={:?}",
        o.stdout,
        o.stderr
    );
    assert!(
        o.stdout.trim().is_empty(),
        "stdout must be empty (no user-visible systemMessage); stdout={:?} stderr={:?}",
        o.stdout,
        o.stderr
    );
    let log = f.home.join(".donegate/state/log.jsonl");
    let body = std::fs::read_to_string(&log)
        .unwrap_or_else(|e| panic!("durable trace {} must exist: {e}", log.display()));
    let root_s = root.display().to_string();
    let line = body.lines().find(|l| {
        serde_json::from_str::<serde_json::Value>(l)
            .ok()
            .is_some_and(|v| {
                v.get("verdict").and_then(|x| x.as_str()) == Some("unconfigured")
                    && v.get("session").and_then(|x| x.as_str()) == Some("s-nocfg")
            })
    });
    let line = line.unwrap_or_else(|| {
        panic!("log.jsonl needs a verdict=unconfigured line for session s-nocfg; log={body:?}")
    });
    assert!(
        line.contains(&root_s),
        "unconfigured log line must name the root {root_s}; line={line:?}"
    );
}

#[test]
fn no_config_wrote_files_allows_quietly_and_logs_trace() {
    let f = Fixture::new(None);
    let tp = f.transcript(&f.primary.join("session_edit.rs"));
    let o = f.stop(&f.primary, &tp);
    assert_quiet_allow_with_trace(&f, &o, &f.primary);
}

#[test]
fn no_config_linked_worktree_allows_quietly_and_logs_trace() {
    let f = Fixture::new(None);
    let tp = f.transcript(&f.wt.join("session_edit.rs"));
    let o = f.stop(&f.primary, &tp);
    assert_quiet_allow_with_trace(&f, &o, &f.wt);
}

/// CONTROL (already implemented): config exists but declares no checks, the
/// session wrote files -> BLOCK.
#[test]
fn config_present_empty_check_set_still_blocks() {
    let f = Fixture::new(Some("max_attempts = 50\n"));
    let tp = f.transcript(&f.primary.join("session_edit.rs"));
    let o = f.stop(&f.primary, &tp);
    assert_eq!(
        o.field("decision"),
        "block",
        "config present + empty check set + session writes must block; stdout={:?} stderr={:?}",
        o.stdout,
        o.stderr
    );
}
