#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (2026-11-14, flow-8cca7ef5 orchestrator — not an implementer):
//! a pathlib write/move method reached through a method REFERENCE rather than
//! a direct `Path(..).method(..)` call overwrites /etc/hosts but is Allow.
//!
//! Found by the independent verifier of 17d985f1 (eca769ad) with the hook
//! binary: Allow on that branch AND at main 53916d2a (pre-existing):
//! * unbound call, receiver as first argument:
//!   `Path.replace(Path('/tmp/z'), '/etc/hosts')`
//! * bound method stored in a name, then called: `w = Path('/etc/hosts').write_text; w('x')`
//! * explicit `__call__`: `Path('/etc/hosts').write_text.__call__('x')`
//!
//! Asserted (RED): main is Ask or Deny; subagent is Deny, for each shape.
//! Control (not ignored): the direct `Path('/etc/hosts').write_text('x')` is
//! not Allow.
//!
//! DOES NOT PROVE: getattr / operator.methodcaller / functools.partial
//! spellings; other receivers (os.replace aliases); stdin or file programs.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

struct Home(std::path::PathBuf);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unique_home(tag: &str) -> Home {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bg-pymref-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook; returns the permissionDecision ("allow" on empty stdout).
fn decide(command: &str, home: &Home, subagent: bool) -> String {
    let mut p = serde_json::json!({
        "session_id": "python-write-method-reference-20261114",
        "cwd": home.0.join("wt"),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = serde_json::json!("test-subagent");
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", &home.0)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    println!("[{command:?}] subagent={subagent} => {:?}", stdout.trim());
    if stdout.trim().is_empty() {
        return "allow".into();
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or("?")
        .to_string()
}

const SHAPES: &[(&str, &str)] = &[
    (
        "unbound-replace",
        "python3 -c \"from pathlib import Path; Path.replace(Path('/tmp/z'), '/etc/hosts')\"",
    ),
    (
        "bound-method-name",
        "python3 -c \"from pathlib import Path; w = Path('/etc/hosts').write_text; w('x')\"",
    ),
    (
        "dunder-call",
        "python3 -c \"from pathlib import Path; Path('/etc/hosts').write_text.__call__('x')\"",
    ),
];

#[test]
fn write_method_reference_to_system_path_is_not_allowed() {
    let home = unique_home("red");
    let mut allowed = Vec::new();
    for (name, cmd) in SHAPES {
        let main = decide(cmd, &home, false);
        let sub = decide(cmd, &home, true);
        if !(main == "ask" || main == "deny") {
            allowed.push(format!("[{name}] main -> {main}"));
        }
        if sub != "deny" {
            allowed.push(format!("[{name}] subagent -> {sub}"));
        }
    }
    assert!(
        allowed.is_empty(),
        "{} of {} (shape x mode) runs were not restricted:\n{}",
        allowed.len(),
        SHAPES.len() * 2,
        allowed.join("\n")
    );
}

/// Round-2 RED (2026-11-14, flow-8cca7ef5 orchestrator — not an implementer):
/// the bound method stored as a CLASS attribute and called through the class
/// was Allow on the round-1 fix a8845f1e and at main 916a29bd (main and
/// subagent; probe run/orch-c8b-probe.txt, row class-body).
#[test]
fn class_attribute_method_reference_to_system_path_is_not_allowed() {
    let home = unique_home("cls");
    let cmd = "python3 -c \"from pathlib import Path\nclass C:\n    w = Path('/etc/hosts').write_text\nC.w('x')\"";
    let main = decide(cmd, &home, false);
    let sub = decide(cmd, &home, true);
    assert!(
        (main == "ask" || main == "deny") && sub == "deny",
        "main={main} subagent={sub}"
    );
}

#[test]
fn control_direct_write_text_is_not_allowed() {
    let home = unique_home("ctl");
    let cmd = "python3 -c \"from pathlib import Path; Path('/etc/hosts').write_text('x')\"";
    assert_ne!(decide(cmd, &home, false), "allow");
    assert_eq!(decide(cmd, &home, true), "deny");
}
