#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (observed 2026-11-14 by independent verifier): after t3 change, these went
//! from allow/allow (main/subagent) at baseline to deny/deny in the worktree:
//! - `exec <<< 'chmod 777 /etc/hosts'; bash script.sh`
//! - `exec <<< 'chmod 777 /etc/hosts'; bash -c 'echo hi'`
//! - `exec <<< 'chmod 777 /etc/hosts'; exec 0<&-; bash` (was ask/deny -> now deny/deny)
//!
//! Two readings: (1) real FP (false positive): the here-string text is never
//! executed because the reader (bash, bash -c, bash reading from fd 3) does not
//! take its program from stdin (fd 0). The exec <<< redirects fd 0 to the
//! here-string, but bash script.sh ignores stdin entirely; (2) intentional
//! over-approximation: treat any shell/interpreter invocation after exec <<<
//! harmful as if it might read the here-string, even if it demonstrably does not.
//! Tell apart: the real FP view says we need to reason about *which fd* the reader
//! draws program bytes from, not just "there is a reader"; the over-approximation
//! says fd 0 is "potentially live" so Block. Current t3 impl (crates/blastguard/src/detect/payload/exec_fds.rs ~897-912)
//! fire the fd-0 branch for any pipeline_readers (shell/interpreter), not only
//! readers whose source is stdin.
//!
//! Asserted: all four cases must NOT be "deny" in both modes (baseline was allow/allow
//! for cases 1-2, and ask/deny for case 3; regression is they became deny/deny).
//! Cases 1-2 should be "allow" in both modes. Case 3 should not be "deny" in main.
//! Controls must hold: harmful via stdin should stay deny; non-harmful asks should
//! hold their baseline.
//!
//! DOES NOT PROVE: the exact fix mechanism (how to distinguish readers by source fd).
//! DOES NOT PROVE: whether over-approximation is the intended design or a bug.
//! It proves only: the current impl produces a regression (false denies where baseline
//! allowed or asked).
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
        "bg-fp1114-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "side-findings-20261114-fp",
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

#[test]
fn exec_stdin_herestring_harmful_bash_script_file_should_allow_in_both_modes() {
    let home = unique_home("red1");
    let cmd = "exec <<< 'chmod 777 /etc/hosts'; bash script.sh";
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    // Re-anchored allow -> Undetermined: user ruling 2026-11-18 (S4 B.5),
    // SUBSET.md ruling 10 (`script.sh` is untracked in the fixture). The
    // FP this test pins still holds: the reason is the B.5 program-file
    // one, not the exec-stdin harm (`bash` reading the herestring).
    assert_eq!(m, "ask", "main reason={mr}");
    assert_eq!(s, "deny", "subagent reason={sr}");
    for r in [&mr, &sr] {
        assert!(
            r.contains("runs the program file `script.sh`") && r.contains("Table B.5"),
            "reason={r}"
        );
        assert!(!r.contains("chmod"), "reason={r}");
    }
}

#[test]
fn exec_stdin_herestring_harmful_bash_c_should_allow_in_both_modes() {
    let home = unique_home("red2");
    let cmd = "exec <<< 'chmod 777 /etc/hosts'; bash -c 'echo hi'";
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    assert_eq!(m, "allow", "main reason={mr}");
    assert_eq!(s, "allow", "subagent reason={sr}");
}

#[test]
fn exec_stdin_herestring_harmful_stdin_closed_should_not_deny_in_main() {
    let home = unique_home("red3");
    let cmd = "exec <<< 'chmod 777 /etc/hosts'; exec 0<&-; bash";
    let (m, mr) = run_as(cmd, &home, false);
    assert_ne!(
        m, "deny",
        "main should not be deny (baseline was ask); reason={mr}"
    );
}

#[test]
fn control_exec_stdin_herestring_harmful_bash_direct_should_remain_deny_both_modes() {
    let home = unique_home("ctl1");
    let cmd = "exec <<< 'chmod 777 /etc/hosts'; bash";
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    assert_eq!(m, "deny", "main reason={mr}");
    assert_eq!(s, "deny", "subagent reason={sr}");
}

#[test]
fn control_exec_stdin_herestring_benign_bash_baseline_should_hold() {
    let home = unique_home("ctl2");
    let cmd = "exec <<< 'echo hi'; bash";
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    assert_eq!(m, "ask", "main should be ask; reason={mr}");
    assert_eq!(s, "deny", "subagent should be deny; reason={sr}");
}
