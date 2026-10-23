#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (R1): inject mode, after an already-reviewed allow, a CHANGED
//! unreviewed diff of the same file(s) is let through.
//!
//! Sequence: edit -> block (round 1) -> same diff -> allow already-reviewed
//! (attempts reset to 0) -> edit again -> the new diff hashes differently, so
//! evaluate() returns blocked-inject with attempts=1, whose reason text is
//! byte-identical to round 1. main.rs routes it through
//! harness_core::repeat::emit_stop_block, whose fingerprint is the reason text,
//! so the ledger waives it with only a systemMessage while log.jsonl still says
//! blocked-inject. A changed, unreviewed diff must be blocked.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn unique_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!(
        "reviewgate-repro-{}-{}-{}",
        tag,
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("create scratch dir");
    p
}

fn git(work: &Path, args: &[&str]) {
    let st = Command::new("git")
        .current_dir(work)
        .args(args)
        .status()
        .expect("git runs");
    assert!(st.success(), "git {args:?} failed");
}

fn init_repo(work: &Path) {
    git(work, &["init", "-q"]);
    git(work, &["config", "user.email", "t@t.com"]);
    git(work, &["config", "user.name", "t"]);
    std::fs::write(work.join("a.rs"), "fn a() {}\n").unwrap();
    git(work, &["add", "a.rs"]);
    git(work, &["commit", "-qm", "init"]);
}

/// One Stop-hook run in a child process. Returns (exit code, stdout, stderr).
fn run_review(cwd: &Path, home: &Path, session: &str) -> (i32, String, String) {
    ensure_inject_config(home);
    let payload = format!(
        r#"{{"hook_event_name":"Stop","session_id":"{session}","stop_hook_active":false,"cwd":{}}}"#,
        serde_json::to_string(&cwd.to_string_lossy()).unwrap()
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_reviewgate"))
        .arg("review")
        .current_dir(cwd)
        .env("HOME", home)
        .env("CLAUDE_CODE_SESSION_ID", session)
        .env_remove("REVIEWGATE_DISABLE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn is_block(stdout: &str) -> bool {
    stdout.contains("\"decision\":\"block\"") || stdout.contains("\"decision\": \"block\"")
}

fn log_verdicts(home: &Path) -> Vec<String> {
    std::fs::read_to_string(home.join(".reviewgate/state/log.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| v["verdict"].as_str().map(str::to_string))
        .collect()
}

#[test]
fn changed_diff_after_already_reviewed_is_blocked_not_waived() {
    let root = unique_dir("r1");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    init_repo(&work);
    let session = "repro-r1-session";

    std::fs::write(work.join("a.rs"), "fn a() -> u8 { 1 }\n").unwrap();
    let (_, o1, e1) = run_review(&work, &home, session);
    assert!(
        is_block(&o1),
        "control: first review of a real edit must block. stdout={o1:?} stderr={e1:?}"
    );

    let (_, o2, e2) = run_review(&work, &home, session);
    assert!(
        !is_block(&o2),
        "control: the same diff must be allowed as already-reviewed. stdout={o2:?} stderr={e2:?}"
    );

    std::fs::write(
        work.join("a.rs"),
        "fn a() -> u8 { 2 } // NEW UNREVIEWED CHANGE\n",
    )
    .unwrap();
    let (_, o3, e3) = run_review(&work, &home, session);
    let verdicts = log_verdicts(&home);
    assert!(
        is_block(&o3),
        "a CHANGED, never-reviewed diff must be blocked. It was waived: stdout={o3:?} stderr={e3:?};          log.jsonl verdicts={verdicts:?} (the log claims blocked-inject while the hook let the stop through)"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// reviewgate's built-in default is `mode = "off"` (no review). These tests
/// exercise the reviewing path, so they opt in explicitly with a home config
/// `mode = "inject"` -- written only when the test has not placed its own.
fn ensure_inject_config(home: &Path) {
    let cfg = home.join(".reviewgate").join("config.toml");
    if !cfg.exists() {
        std::fs::create_dir_all(home.join(".reviewgate")).unwrap();
        std::fs::write(cfg, "mode = \"inject\"\n").unwrap();
    }
}
