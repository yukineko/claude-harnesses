#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (R2): the inject-mode give-up (review.rs, attempts > max_attempts)
//! allows with tag "giveup" and emits no visible signal: no stderr warning, no
//! stdout message. A forced give-up lets an unreviewed diff through and must be
//! surfaced.

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
fn inject_giveup_is_visibly_signalled() {
    let root = unique_dir("r2");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    init_repo(&work);
    let session = "repro-r2-session";

    std::fs::write(work.join("a.rs"), "fn a() -> u8 { 1 }\n").unwrap();
    let (_, o1, e1) = run_review(&work, &home, session);
    assert!(
        is_block(&o1),
        "control: round 1 must block. stdout={o1:?} stderr={e1:?}"
    );

    // default max_attempts is 2: round 2 still blocks, round 3 is the give-up.
    std::fs::write(work.join("a.rs"), "fn a() -> u8 { 2 } // changed again\n").unwrap();
    let (_, o1b, e1b) = run_review(&work, &home, session);
    assert!(
        is_block(&o1b),
        "control: round 2 must block. stdout={o1b:?} stderr={e1b:?}"
    );

    std::fs::write(
        work.join("a.rs"),
        "fn a() -> u8 { 3 } // changed a third time\n",
    )
    .unwrap();
    let (code, o2, e2) = run_review(&work, &home, session);
    let verdicts = log_verdicts(&home);
    assert_eq!(code, 0);
    assert!(
        verdicts.last().map(String::as_str) == Some("giveup"),
        "setup check: round 3 should be the inject giveup. verdicts={verdicts:?} stdout={o2:?} stderr={e2:?}"
    );
    let all = format!("{o2}\n{e2}").to_lowercase();
    assert!(
        all.contains("giveup") || all.contains("give up") || all.contains("gave up") || all.contains("max_attempts"),
        "the give-up allowed an unreviewed diff but printed no signal naming it. stdout={o2:?} stderr={e2:?}"
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
