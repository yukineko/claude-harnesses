#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (fail-open, side finding F13 observed 2026-11-15 on base 0.2.603):
//!   echo 'import os; os.chmod("/etc/hosts", 0o777)' \
//!     | python3 -c 'import os; os.system("python3 -m asyncio")'
//! was observed allow in main and subagent mode. The outer program only
//! spawns a child `python3 -m asyncio`; the child is an interactive REPL and
//! inherits the outer process's stdin, so it executes the piped harmful
//! program.
//!
//! Two readings: (1) real defect: an inner interpreter started with no
//! script/-c argument reads its program from the inherited stdin, so the
//! piped text is the program and system harm must be deny exactly as for a
//! direct `... | python3 -m asyncio`; (2) intended: what a spawned child
//! reads from stdin is out of scope. Tell apart: the user ruling is that
//! system harm is deny through any wrapper, and `os.system` is a wrapper the
//! detector already descends into -> reading (1). Benign stdin must not be
//! deny (control).
//!
//! Asserted: harmful stdin -> deny in main and subagent mode.
//!
//! DOES NOT PROVE: other REPL spellings (`python3 -i`, `node`, `ruby -e`
//! none, `bash`), `subprocess.run`/`os.popen` wrappers, non-stdin pipes, or the
//! reason text. The control shows only that the same command with benign
//! stdin `print(1)` is not deny.
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
        "bg-side1115-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "side-findings-20261115",
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

fn nested_repl(stdin_text: &str) -> String {
    format!(r#"echo '{stdin_text}' | python3 -c 'import os; os.system("python3 -m asyncio")'"#)
}

#[test]
#[ignore = "RED: 2026-11-15 F13 harmful stdin into os.system nested REPL is allow — remove when fixed"]
fn harmful_stdin_into_os_system_nested_repl_is_deny() {
    let cmd = nested_repl(r#"import os; os.chmod("/etc/hosts", 0o777)"#);
    let home = unique_home("a");
    let (m, mr) = run_as(&cmd, &home, false);
    let (s, sr) = run_as(&cmd, &home, true);
    assert_eq!(m, "deny", "main must deny; cmd={cmd:?} reason={mr}");
    assert_eq!(s, "deny", "subagent must deny; cmd={cmd:?} reason={sr}");
}

#[test]
fn control_benign_stdin_into_nested_repl_is_not_deny() {
    let cmd = nested_repl("print(1)");
    let home = unique_home("ctl");
    let (m, mr) = run_as(&cmd, &home, false);
    let (s, sr) = run_as(&cmd, &home, true);
    assert_ne!(m, "deny", "main; cmd={cmd:?} reason={mr}");
    assert_ne!(s, "deny", "subagent; cmd={cmd:?} reason={sr}");
}
