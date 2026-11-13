#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (fail-open, observed 2026-11-13 at main 15b5c10e): a Python `os.*`
//! path call whose system path is spelled with a string-literal ESCAPE is
//! Allow, while the plain literal is denied:
//! `python3 -c "import os; os.chmod('\x2fetc/hosts',0)"` is Allow,
//! `python3 -c "import os; os.chmod('/etc/hosts',0)"` is Deny.
//!
//! Python decodes `\xHH`, octal `\ooo`, `\uXXXX` and `\N{NAME}` in a non-raw
//! literal at compile time, so `'\x2fetc/hosts'` IS `'/etc/hosts'`.
//! Found by the 47b20647 round-2 verifier (`\N{SOLIDUS}` on os.chmod) and
//! measured by the orchestrator (non-implementer) at the 47b20647 branch tip,
//! where the same shapes are Allow too (that branch decodes escapes for
//! `open()` but the os.* path-call judge does not see the decoded value).
//!
//! Asserted: no listed program is Allow, in the main session and in a
//! subagent. Control (not ignored): the plain-literal twins are denied.
//!
//! DOES NOT PROVE: shapes beyond those listed (other os/shutil functions,
//! bytes literals, dynamic construction) are judged; nor what the decision
//! reason says.

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
        "bg-ped-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "python-escape-os-path-20261113",
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
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

const ESCAPED: &[&str] = &[
    "python3 -c \"import os; os.chmod('\\x2fetc/hosts',0)\"",
    "python3 -c \"import os; os.chmod('\\057etc/hosts',0)\"",
    "python3 -c \"import os; os.chmod('\\u002fetc/hosts',0)\"",
    "python3 -c \"import os; os.chmod('\\N{SOLIDUS}etc/hosts',0)\"",
    "python3 -c \"import os; os.rename('/tmp/a','\\N{SOLIDUS}etc/hosts')\"",
    "python3 -c \"import os; os.rename('/tmp/a','\\x2fetc/hosts')\"",
];

const PLAIN: &[&str] = &[
    "python3 -c \"import os; os.chmod('/etc/hosts',0)\"",
    "python3 -c \"import os; os.rename('/tmp/a','/etc/hosts')\"",
];

#[test]
#[ignore = "RED: python escape-decoded system path in os.* call is Allow (backlog p0)"]
fn escape_decoded_system_path_in_os_call_is_not_allowed() {
    let home = unique_home("esc");
    let mut allowed = Vec::new();
    for c in ESCAPED {
        for subagent in [false, true] {
            let (d, _) = run_as(c, &home, subagent);
            if d == "allow" {
                allowed.push(format!("subagent={subagent} {c}"));
            }
        }
    }
    assert!(
        allowed.is_empty(),
        "{} of {} (shape x mode) escape-spelled system-path os.* calls were ALLOWED:\n{}",
        allowed.len(),
        ESCAPED.len() * 2,
        allowed.join("\n")
    );
}

#[test]
fn control_plain_literal_twins_are_denied() {
    let home = unique_home("ctl");
    for c in PLAIN {
        for subagent in [false, true] {
            let (d, r) = run_as(c, &home, subagent);
            assert_eq!(d, "deny", "control {c} subagent={subagent}: {d} {r}");
        }
    }
}
