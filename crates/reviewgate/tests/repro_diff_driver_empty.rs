#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (R3): a reviewable change whose git diff driver prints nothing
//! (.gitattributes "*.rs diff=nop" + diff.nop.command=true) reads as an empty
//! diff: logged "empty-diff" with files [] and allowed. A real change whose
//! content could not be obtained is undetermined, not clean.

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
#[ignore = "RED repro: reviewgate allows a real change as empty-diff when the git diff driver prints nothing"]
fn silent_diff_driver_change_is_not_allowed_as_empty() {
    let root = unique_dir("r3");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    init_repo(&work);
    git(&work, &["config", "diff.nop.command", "true"]);
    std::fs::write(work.join(".gitattributes"), "*.rs diff=nop\n").unwrap();
    git(&work, &["add", ".gitattributes"]);
    git(&work, &["commit", "-qm", "attrs"]);
    std::fs::write(
        work.join("a.rs"),
        "fn a() -> u8 { 1 } // REAL UNREVIEWED CHANGE\n",
    )
    .unwrap();

    let (code, o, e) = run_review(&work, &home, "repro-r3-session");
    let verdicts = log_verdicts(&home);
    assert_eq!(code, 0);
    assert!(
        is_block(&o),
        "git reports a.rs as changed but the diff text came back empty; that must not be allowed as clean.          stdout={o:?} stderr={e:?} verdicts={verdicts:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
