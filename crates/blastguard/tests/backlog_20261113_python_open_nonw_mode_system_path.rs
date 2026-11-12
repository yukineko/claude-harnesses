#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (CRITICAL fail-open, observed 2026-11-13 at main d37ae072): a
//! `python3 -c` program that opens a SYSTEM-directory file for writing in any
//! mode other than the `'w'`-family spellings blastguard lists is Allowed.
//!
//! `python3 -c "open('/etc/hosts','w').write('x')"` is denied ("it writes
//! `/etc/hosts` (`,'w')`) ..."), but `open('/etc/hosts','a').write('x')` is
//! allowed in the main session and in a subagent. `INTERPRETER_WRITE_CALLS`
//! (crates/blastguard/src/detect/interpreter.rs) lists `,'w')`, `,'wb')`,
//! `,'w+')`, ... but no append (`'a'`, `'a+'`, `'ab'`), update (`'r+'`),
//! exclusive-create (`'x'`) or keyword (`mode='a'`) spelling, and the
//! system-path mutator rule only covers calls like os.chmod/os.remove.
//!
//! Asserted: none of these modes on a system-directory target is Allow.
//! Control (GREEN at HEAD, not ignored): `'w'` on the same path stays denied.
//!
//! DOES NOT PROVE: anything about non-system targets (append to a git-tracked
//! cwd file may legitimately be allowed), nor other interpreters.

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
        "bg-pymode-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "py-open-mode-20261113",
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

const WRITING_MODES: &[&str] = &[
    "python3 -c \"open('/etc/hosts','a').write('x')\"",
    "python3 -c \"open('/etc/hosts','a+').write('x')\"",
    "python3 -c \"open('/etc/hosts','ab').write(b'x')\"",
    "python3 -c \"open('/etc/hosts','r+').write('x')\"",
    "python3 -c \"open('/etc/hosts','w+').write('x')\"",
    // 'x' creates a new file: a not-yet-existing name in a system directory.
    "python3 -c \"open('/etc/blastguard-red-x','x').write('x')\"",
    "python3 -c \"open('/etc/hosts',mode='a').write('x')\"",
];

// test-weakening-justified: new RED for an unfixed CRITICAL fail-open (backlog
// pending, 2026-11-13); the ignore is removed in the fixing commit.
#[test]
#[ignore = "RED: <backlog-pending> 2026-11-13 python open() in append/update/create mode on a system path is allowed — remove the ignore when fixed"]
fn python_open_writing_modes_on_system_path_are_not_allowed() {
    let home = unique_home("red");
    let mut failures = Vec::new();
    for c in WRITING_MODES {
        for subagent in [false, true] {
            let (d, r) = run_as(c, &home, subagent);
            if d == "allow" {
                failures.push(format!("subagent={subagent} {c:?} -> {d} | {r}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} python open() write(s) to a system path were ALLOWED:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// Control: 'w' on the same path is denied.
#[test]
fn control_w_mode_on_system_path_is_denied() {
    let home = unique_home("ctl");
    for subagent in [false, true] {
        let c = "python3 -c \"open('/etc/hosts','w').write('x')\"";
        let (d, r) = run_as(c, &home, subagent);
        assert_eq!(d, "deny", "subagent={subagent} {c:?} -> {d} | {r}");
    }
}
