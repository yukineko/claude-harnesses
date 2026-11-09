// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (2026-11-07): the repeat identity must not depend on the check's runtime.
//! The check prints identical output both times but sleeps only on the first run
//! (marker file), so the block reason's `(exit 1, <secs>s)` differs. The 2nd stop
//! must still be waived as a repeat. Flake seen once in
//! `quiet_feedback_more::a_repeat_is_recognised_when_only_the_record_path_differs`.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("donegate-qm-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch");
    dir
}

struct Fixture {
    home: PathBuf,
    root: PathBuf,
}

/// A trusted project with one required check that prints `raw.txt` and exits 1.
/// `extra` is extra top-level toml (before the `[[check]]` table).
fn fixture(tag: &str, raw: &str, extra: &str) -> Fixture {
    let home = scratch(tag);
    let root = home.join("project");
    let sd = home.join("sd");
    std::fs::create_dir_all(&root).expect("create project root");
    std::fs::write(root.join("raw.txt"), raw).expect("write raw.txt");
    std::fs::write(
        root.join("donegate.toml"),
        format!(
            "max_attempts = 50\nstate_dir = \"{}\"\n{extra}\n[[check]]\nname = \"chk\"\ncmd = \"cat raw.txt; if [ ! -f slow.done ]; then sleep 0.25; touch slow.done; fi; exit 1\"\n",
            sd.display()
        ),
    )
    .expect("write donegate.toml");
    let canon = std::fs::canonicalize(&root).expect("canonicalize project root");
    std::fs::create_dir_all(home.join(".harness")).expect("create ~/.harness");
    std::fs::write(
        home.join(".harness/trust.toml"),
        format!("trusted = [\"{}\"]\n", canon.display()),
    )
    .expect("write trust.toml");
    Fixture { home, root }
}

struct Run {
    stdout: String,
    stderr: String,
}

fn stop(f: &Fixture, session: &str) -> Run {
    let payload = serde_json::json!({
        "session_id": session,
        "cwd": f.root.to_string_lossy(),
        "hook_event_name": "Stop",
    })
    .to_string();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_donegate"));
    harness_core::git_env::scrub(&mut cmd);
    cmd.arg("gate")
        .current_dir(&f.root)
        .env("HOME", &f.home)
        .env("CLAUDE_CODE_SESSION_ID", session)
        .env_remove("DONEGATE_DISABLE")
        .env_remove("HARNESS_TRUST_ALL");
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("donegate spawns");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("donegate runs");
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn block_reason(r: &Run) -> Option<String> {
    for line in r.stdout.lines() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if v.get("decision").and_then(|d| d.as_str()) == Some("block") {
                return v.get("reason").and_then(|x| x.as_str()).map(String::from);
            }
        }
    }
    None
}

fn reason_of(r: &Run) -> String {
    block_reason(r).unwrap_or_else(|| {
        panic!(
            "the stop was not blocked.\n--- stdout ---\n{}\n--- stderr ---\n{}",
            r.stdout, r.stderr
        )
    })
}

fn waived(r: &Run) -> bool {
    block_reason(r).is_none() && r.stdout.contains("systemMessage")
}

#[test]
fn a_repeat_is_recognised_when_only_the_check_duration_differs() {
    let f = fixture(
        "rep-dur",
        "boom: always the same\n",
        "reset_after_secs = 1\n",
    );
    let r1 = stop(&f, "s-rep-dur");
    let reason1 = reason_of(&r1);
    std::thread::sleep(std::time::Duration::from_millis(2300));
    let r2 = stop(&f, "s-rep-dur");
    assert!(
        waived(&r2),
        "the 2nd identical failure (only runtime differs) must be waived as a repeat\n--- 1st reason ---\n{reason1}\n--- 2nd stdout ---\n{}\n--- stderr ---\n{}",
        r2.stdout,
        r2.stderr
    );
}
