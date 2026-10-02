#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog a5bc063a: donegate has two give-up paths. The checks-red one
//! (538a8ac8) records `donegate:giveup:<check>` in the overwatch violation
//! ledger. The UNDETERMINED one — `refuse()` reaching the attempt cap because
//! the project's declaration could not be judged (untrusted / unreadable
//! donegate.toml) — only writes donegate's private JSONL + stderr
//! ("Allowing stop; NOTHING WAS VERIFIED.") and exits 0. Nothing an outside
//! reader consults says the gate gave up, so this give-up is indistinguishable
//! from a pass downstream — the heavier of the two cases was left unhardened.
//!
//! Real binary, `$HOME` set per child (no real trust.toml / ~/.overwatch).

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

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
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Every `violations.jsonl` under the isolated HOME, read off disk.
fn violation_ledger(dir: &Path, out: &mut String) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            violation_ledger(&p, out);
        } else if p.file_name().is_some_and(|n| n == "violations.jsonl") {
            out.push_str(&std::fs::read_to_string(&p).unwrap_or_default());
        }
    }
}

#[test]
#[ignore = "backlog a5bc063a: open defect, remove ignore when fixed"]
fn refusal_giveup_at_the_attempt_cap_leaves_a_durable_trace() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let root = tmp.path().join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    // A declared check in an UNTRUSTED project: donegate refuses to judge.
    std::fs::write(
        root.join("donegate.toml"),
        "max_attempts = 3\n\n[[check]]\nname = \"typecheck\"\ncmd = \"exit 0\"\n",
    )
    .unwrap();

    let session = "sess-a5bc063a";
    let mut gave_up = None;
    let mut refused_seen = false;
    for i in 1..=10 {
        let (code, stdout, stderr) = run_gate(&home, &root, session);
        assert_eq!(code, 0, "Stop hook exits 0 toward Claude: {stderr}");
        if stderr.contains("still unable to judge") {
            gave_up = Some(i);
            break;
        }
        refused_seen |= stdout.contains("block") || stderr.contains("donegate");
    }
    assert!(
        refused_seen,
        "precondition: the untrusted declaration was refused"
    );
    assert!(
        gave_up.is_some(),
        "precondition: the refusal reached the attempt-cap give-up"
    );

    let mut ledger = String::new();
    violation_ledger(&home, &mut ledger);
    assert!(
        ledger.contains("giveup"),
        "refuse() gave up at the attempt cap (attempt {gave_up:?}) and allowed the stop, but \
         left nothing in the overwatch violation ledger; ledger={ledger:?}"
    );
}
