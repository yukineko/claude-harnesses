// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! End-to-end: the REFUSAL give-up (unable to judge, attempt cap exhausted)
//! must leave a durable overwatch trace, like the checks-red give-up does
//! (backlog a5bc063a; twin of 538a8ac8 / giveup_sentinel.rs).
//!
//! The project carries a `donegate.toml` but is NOT trusted, so donegate
//! refuses to judge it (`Declaration::RefusedUntrusted`). After `max_attempts`
//! refusals it allows the stop — that concession is pinned, not changed.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn scratch(tag: &str) -> PathBuf {
    // Unique per call: a leftover dir from a recycled pid would carry a stale
    // ledger into the assertions.
    let dir = std::env::temp_dir().join(format!(
        "donegate-giveup-refusal-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("create scratch");
    dir
}

fn run_gate(home: &Path, root: &Path, session: &str) -> (i32, String, String) {
    let payload = format!(
        r#"{{"session_id":"{session}","cwd":"{}","hook_event_name":"Stop"}}"#,
        root.display()
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_donegate"))
        .arg("gate")
        .current_dir(root)
        .env("HOME", home)
        .env("CLAUDE_CODE_SESSION_ID", session)
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
    let out = child.wait_with_output().expect("donegate runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Every `violations.jsonl` under the isolated `$HOME`, read straight off disk.
fn violation_ledger(home: &Path) -> String {
    fn walk(dir: &Path, out: &mut String) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.file_name().is_some_and(|n| n == "violations.jsonl") {
                out.push_str(&std::fs::read_to_string(&p).unwrap());
            }
        }
    }
    let mut out = String::new();
    walk(home, &mut out);
    out
}

#[test]
fn refusal_attempt_cap_leaves_a_durable_giveup_sentinel() {
    let home = scratch("untrusted");
    let root = home.join("project");
    std::fs::create_dir_all(&root).unwrap();
    // Declared but untrusted: donegate refuses to judge (no trust.toml at all).
    std::fs::write(
        root.join("donegate.toml"),
        "[[check]]\nname = \"typecheck\"\ncmd = \"exit 0\"\n",
    )
    .unwrap();
    let session = "sess-refusal-cap";

    let mut gave_up = false;
    for i in 1..=20 {
        let (code, stdout, stderr) = run_gate(&home, &root, session);
        assert_eq!(
            code, 0,
            "a Stop hook exits 0 toward Claude; stderr={stderr:?}"
        );
        if stderr.contains("still unable to judge") {
            assert!(
                !stdout.contains("\"decision\""),
                "IN SCOPE-CHECK: the cap must still allow the stop. stdout={stdout:?}"
            );
            gave_up = true;
            break;
        }
        // CONTROL: while still refusing (blocking), no give-up marker may exist.
        assert!(
            stdout.contains("\"decision\"") || stdout.contains("systemMessage"),
            "attempt {i}: apparatus — expected a refusal block; stdout={stdout:?} stderr={stderr:?}"
        );
        let ledger = violation_ledger(&home);
        assert!(
            !ledger.contains("giveup"),
            "attempt {i}: a give-up sentinel must not appear before the cap. ledger={ledger:?}"
        );
    }
    assert!(gave_up, "apparatus: the refusal give-up branch never ran");

    let ledger = violation_ledger(&home);
    assert!(
        ledger.contains("donegate:giveup:refusal:untrusted"),
        "the gate gave up WITHOUT judging and left no durable trace in the overwatch ledger \
         (only donegate's private JSONL). ledger={ledger:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
}
