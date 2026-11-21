//! RED (2026-11-22, observed at main 0a3c2df02 in condukt run
//! run-20261122-025656-99560, relay bgr-3b84de93184d): a `for` loop that feeds
//! each file on stdin to a LITERAL absolute program run under `env -i VAR=…`
//! inside `$(…)` (`env -i HOME=<dir> PATH=… /abs/prog < "$f"`) is refused
//! (measured here: ask for a main session; the plain `/abs/prog < "$f"` loop
//! without `env -i` is Allow) with "the command executed here comes from `"$f"`, whose value only
//! exists at run time — blastguard cannot see what would actually run". `"$f"`
//! is the stdin source of the redirect, not the command word: the program run
//! is the literal path, which blastguard can see.
//!
//! Exact relayed command (paths shortened to a fixture dir; same shape):
//! `for f in <dir>/l_*; do o=$(env -i HOME=<dir> PATH=/usr/bin:/bin /usr/bin/wc
//! -c < "$f" 2>/dev/null | grep -o x); echo "${o:-allow}"; done | paste - - | nl`
//!
//! Passing means: each shape below is Allow for a main session.
//!
//! Readings: (a) FP: the redirect operand is read as the command word;
//! (b) intended: a runtime-valued stdin source is undetermined. Tell apart: the
//! command run is the literal `/usr/bin/wc`; reading a file on stdin is not
//! executing it. No ruling for (b) found.
//!
//! DOES NOT PROVE: subagent sessions (the same verdict would surface as Deny);
//! a runtime-valued COMMAND word (`"$f" arg`), which must stay undetermined;
//! protected-path stdin sources.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

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
        "bg-stdinloopvar-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn decide(command: &str, home: &Home) -> String {
    let p = serde_json::json!({
        "session_id": "stdin-loop-var-20261122",
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
        return "allow".into();
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or("?")
        .to_string()
}

#[test]
fn control_literal_stdin_source_is_allow() {
    let home = unique_home("ctl");
    let d = home.0.join("d");
    let cmd = format!("/usr/bin/wc -c < {}/a", d.display());
    assert_eq!(decide(&cmd, &home), "allow", "{cmd}");
}

#[test]
#[ignore = "RED(3775027f): a runtime-valued stdin source (< \"$f\") is read as the command word"]
fn loop_var_stdin_source_to_literal_program_is_allow() {
    let home = unique_home("loop");
    let d = home.0.join("d");
    let d = d.display();
    let mut bad = Vec::new();
    for cmd in [
        format!("for f in {d}/l_*; do /usr/bin/wc -c < \"$f\"; done"),
        format!(
            "for f in {d}/l_*; do o=$(env -i HOME={d} PATH=/usr/bin:/bin /usr/bin/wc -c < \"$f\" 2>/dev/null | grep -o x); echo \"${{o:-allow}}\"; done | paste - - | nl"
        ),
    ] {
        let got = decide(&cmd, &home);
        if got != "allow" {
            bad.push(format!("{cmd:?} => {got}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
