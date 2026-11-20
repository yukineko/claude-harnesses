#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (possible false positive, observed 2026-11-20, flow session, blastguard
//! 0.2.798): a `while IFS='|' read -r ...; do ...; done <<'EOF'` loop whose
//! heredoc DATA lines contained the text `echo hi >&$fd` was Denied with
//! "'> $fd' destroys the file's current contents and `$fd` contains a shell
//! expansion, so blastguard cannot tell which file this names, let alone
//! whether its contents are recoverable".
//!
//! Two readings: (a) real defect: the heredoc is stdin of the loop and is read
//! by `read` as data; the body only echoes it, so the redirect text must not be
//! judged; (b) intended: blastguard cannot see whether the loop body later
//! executes the data (eval / sh -c), so refusal is conservative.
//! Tell apart: the same loop with harmless data gets verdict V (control i).
//! If the redirect-text data is stricter than V, the data text alone raised it
//! (reading a); if equal, reading (b)-style conservatism is not at play and
//! there is no RED. Control ii (body executes the data) must stay non-allow.
//!
//! Asserted: verdict(loop + redirect-ish data) is no stricter than
//! verdict(same loop + harmless data), in subagent mode and main mode.
//!
//! DOES NOT PROVE: loops whose body executes the data are judged correctly
//! beyond "non-allow"; other readers (`mapfile`, `readarray`); `$fd` forms
//! other than the literal text used.
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
        "bg-side1120-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "side-findings-20261120",
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

fn rank(d: &str) -> u8 {
    match d {
        "allow" => 0,
        "ask" => 1,
        _ => 2,
    }
}

const LOOP_HEAD: &str = "while IFS='|' read -r a b; do echo \"$a $b\"; done";
const HARMLESS: &str = "x|hello\ny|world\n";
const FP_DATA: &str = "x|echo hi >&$fd\ny|probe 3<<< x\n";

fn loop_cmd(data: &str) -> String {
    format!("{LOOP_HEAD} <<'EOF'\n{data}EOF\n")
}

fn check(tag: &str, subagent: bool) {
    let home = unique_home(tag);
    let (cd, cr) = run_as(&loop_cmd(HARMLESS), &home, subagent);
    let (fd, fr) = run_as(&loop_cmd(FP_DATA), &home, subagent);
    eprintln!("MEASURED subagent={subagent} harmless={cd} ({cr}) fp={fd} ({fr})");
    assert!(
        rank(&fd) <= rank(&cd),
        "while-read data heredoc made it stricter (subagent={subagent}): harmless={cd} ({cr}) fp={fd} ({fr})"
    );
}

#[test]
#[ignore = "RED: while-read data heredoc FP, see backlog"]
fn while_read_data_heredoc_fd_redirect_no_stricter_subagent() {
    check("s", true);
}

#[test]
#[ignore = "RED: while-read data heredoc FP, see backlog"]
fn while_read_data_heredoc_fd_redirect_no_stricter_main() {
    check("m", false);
}

#[test]
fn control_harmless_loop_verdict_is_allow() {
    let home = unique_home("h");
    let (d, r) = run_as(&loop_cmd(HARMLESS), &home, true);
    eprintln!("MEASURED harmless-subagent={d} ({r})");
    assert_eq!(d, "allow", "harmless-data loop verdict; reason={r}");
}

#[test]
fn control_body_executes_data_stays_non_allow() {
    let home = unique_home("e");
    let cmd = "while read -r l; do eval \"$l\"; done <<'EOF'\nrm -rf /etc\nEOF\n";
    let (d, r) = run_as(cmd, &home, true);
    eprintln!("MEASURED eval-body={d} ({r})");
    assert_ne!(d, "allow", "eval of heredoc data must stay non-allow; reason={r}");
}

#[test]
fn control_bash_heredoc_fd_redirect_stays_non_allow() {
    let home = unique_home("b");
    let (d, r) = run_as("bash <<'EOF'\necho hi >&$fd\nEOF\n", &home, true);
    eprintln!("MEASURED bash-heredoc={d} ({r})");
    assert_ne!(d, "allow", "bash heredoc must stay non-allow; reason={r}");
}
