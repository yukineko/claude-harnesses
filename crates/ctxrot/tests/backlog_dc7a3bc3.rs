#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro for backlog dc7a3bc3: PreToolUse verdict paths still run under
//! `harness_core::hook::run_hook`, whose barrier turns ANY escaping panic into
//! `exit 0` — and for a PreToolUse hook a silent exit 0 is an ALLOW. harness-core
//! has no shared PreToolUse entry that maps a verdict-path panic to exit 2
//! (blastguard carries a private `run_verdict_guarded`; fetchguard, a
//! PostToolUse twin, and ctxrot's `preguard` still use `run_hook`).
//!
//! `ctxrot preguard`'s own `analyse` barrier covers the detector, but not what
//! surrounds it (`read_stdin`, `HookInput::parse`, `Config::load`, and the final
//! `println!` of the deny). This test drives the last one: a deny decision is
//! reached for an unbounded Read of a 2 MB file, then stdout is a broken pipe,
//! so `println!` panics. The verdict was DENY; the exit status must not say
//! allow.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn scratch() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let d = std::env::temp_dir().join(format!(
        "ctxrot-backlog-dc7a3bc3-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(d.join("home")).unwrap();
    d
}

fn payload(root: &std::path::Path) -> String {
    let big = root.join("huge.log");
    std::fs::write(&big, vec![b'x'; 2_000_000]).unwrap();
    serde_json::json!({
        "session_id": "s-dc7a3bc3",
        "hook_event_name": "PreToolUse",
        "cwd": root,
        "tool_name": "Read",
        "tool_input": { "file_path": big },
    })
    .to_string()
}

fn cmd(root: &std::path::Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ctxrot"));
    c.arg("preguard")
        .env("HOME", root.join("home"))
        .env_remove("GUARD_DISABLE")
        .current_dir(root)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

#[test]
fn backlog_dc7a3bc3_control_preguard_denies_unbounded_huge_read() {
    let root = scratch();
    let mut child = cmd(&root).stdout(Stdio::piped()).spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload(&root).as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let _ = std::fs::remove_dir_all(&root);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("\"permissionDecision\":\"deny\""),
        "control: preguard must deny an unbounded 2MB Read: {out:?}"
    );
}

#[test]
#[ignore = "backlog dc7a3bc3: open defect, remove ignore when fixed"]
fn backlog_dc7a3bc3_panic_on_deny_path_does_not_exit_as_allow() {
    let root = scratch();
    let mut child = cmd(&root).stdout(Stdio::piped()).spawn().unwrap();
    // Break stdout BEFORE the hook can write its deny: println! then panics.
    drop(child.stdout.take());
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload(&root).as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let _ = std::fs::remove_dir_all(&root);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    // Anti-vacuity: the panic really happened on the verdict path.
    assert!(
        err.contains("panic") || err.contains("failed printing"),
        "stderr={err:?}"
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "PreToolUse verdict path panicked after deciding DENY, but exited {:?} \
         (exit 0 = allow for PreToolUse). stderr={err:?}",
        out.status.code()
    );
}
