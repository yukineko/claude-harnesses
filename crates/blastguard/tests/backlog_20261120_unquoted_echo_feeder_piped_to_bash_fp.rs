#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive): a plain unquoted `echo` feeder piped to bash is Ask in
//! main mode, while the program it feeds (`a`, `hi`) is an unknown program
//! name, which the standing ruling resolves to Allow ("unknown program →
//! Allow"). Contrast, both Allow at 0b906f524 (blastguard 0.2.765):
//!   { echo a; echo b; } | bash     allow  (brace-group feeder, backlog 7eb86bb3)
//!   echo 'echo hi' | bash          allow  (quoted plain feeder)
//! so the brace group now reads `a` / `b` as the program and allows it, while
//! the simpler `echo a | bash` does not.
//!
//! DOES NOT PROVE: subagent-mode behaviour (not asserted), nor that a harmful
//! unquoted feeder (`echo rm -rf /etc | bash`) is Deny — a fix must keep that.
//!
//! RED written by an independent test author, not the implementer.

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
        "bg-unquoted-echo-feed-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_main(command: &str, home: &Home) -> (String, String) {
    let p = serde_json::json!({
        "session_id": "unquoted-echo-feeder-fp-20261120",
        "cwd": home.0.join("wt"),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
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

fn not_allowed(cmds: &[&str], tag: &str) -> Vec<String> {
    let home = unique_home(tag);
    cmds.iter()
        .filter_map(|cmd| {
            let (d, r) = run_main(cmd, &home);
            (d != "allow").then(|| format!("{cmd:?} => {d} ({r})"))
        })
        .collect()
}

#[test]
fn unquoted_echo_feeder_piped_to_bash_is_allow() {
    let bad = not_allowed(&["echo a | bash", "echo hi | bash"], "red");
    assert!(bad.is_empty(), "expected Allow:\n{}", bad.join("\n"));
}

#[test]
fn control_group_and_quoted_feeders_are_allow() {
    let bad = not_allowed(
        &["{ echo a; echo b; } | bash", "echo 'echo hi' | bash"],
        "ctl",
    );
    assert!(
        bad.is_empty(),
        "controls must stay Allow:\n{}",
        bad.join("\n")
    );
}
