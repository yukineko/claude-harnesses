#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (observed 2026-11-15, backlog 2210e1db): `exec <<< 'chmod 777 /etc/hosts';
//! bash` was fixed to Deny, but the same program text opened on fd 3 and then
//! duplicated onto stdin, `exec 3<<< 'chmod 777 /etc/hosts'; exec 0<&3; bash`,
//! is not judged as literal system harm.
//!
//! Two readings: (1) real defect: bash reads its program from stdin, which is
//! the fd-3 here-string duplicated onto fd 0; the program text is a literal on
//! the command line and it is system harm; the user ruling is that literal
//! system harm reaching a shell is Deny in main and in subagent mode;
//! (2) intended: an fd-dup'd stdin is undeterminable so Ask/allow is fine.
//! Tell apart: the user ruling (system harm always Deny) decides it; controls
//! show the harmless text, a data reader (cat), and a script operand are not
//! denied, so the fix must track the dup rather than deny every `exec 0<&3`.
//!
//! Asserted: deny in both main and subagent mode for three dup spellings
//! (`0<&3`, `0<&3 3<&-`, `<&3`). Controls: harmless text, `cat`, and
//! `bash script.sh` are not deny.
//!
//! DOES NOT PROVE: other fds than 3, other harm literals, non-shell consumers
//! other than cat, here-docs or file redirects opened on fd N, or that the
//! controls are Allow (only that they are not deny).
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

fn assert_deny_both(tag: &str, cmd: &str) {
    let home = unique_home(tag);
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    assert_eq!(m, "deny", "main reason={mr}");
    assert_eq!(s, "deny", "subagent reason={sr}");
}

fn assert_not_deny(tag: &str, cmd: &str) {
    let home = unique_home(tag);
    for subagent in [false, true] {
        let (d, r) = run_as(cmd, &home, subagent);
        assert_ne!(d, "deny", "subagent={subagent} reason={r}");
    }
}

#[test]
fn dup_fd3_onto_stdin_then_bash_is_deny_in_both_modes() {
    assert_deny_both("dup0", "exec 3<<< 'chmod 777 /etc/hosts'; exec 0<&3; bash");
}

#[test]
fn dup_fd3_onto_stdin_close_fd3_then_sh_is_deny_in_both_modes() {
    assert_deny_both(
        "dupclose",
        "exec 3<<< 'chmod 777 /etc/hosts'; exec 0<&3 3<&-; sh",
    );
}

#[test]
fn dup_fd3_short_form_then_bash_is_deny_in_both_modes() {
    assert_deny_both(
        "dupshort",
        "exec 3<<< 'chmod 777 /etc/hosts'; exec <&3; bash",
    );
}

#[test]
fn control_a_harmless_text_dup_onto_stdin_is_ask_in_main_deny_in_subagent() {
    // user ruling 2026-11-15 (option 1): fd 0 stays unresolved; the benign dup form
    // matches the direct form `exec <<< 'echo hi'; bash` (ask in main / deny in subagent)
    let cmd = "exec 3<<< 'echo hi'; exec 0<&3; bash";
    let home = unique_home("ctla");
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    assert_ne!(m, "deny", "main reason={mr}");
    assert_eq!(m, "ask", "main reason={mr}");
    assert_eq!(s, "deny", "subagent reason={sr}");
}

#[test]
fn control_b_cat_reads_stdin_as_data_is_not_deny() {
    assert_not_deny("ctlb", "exec 3<<< 'chmod 777 /etc/hosts'; exec 0<&3; cat");
}

#[test]
fn control_c_bash_with_script_operand_is_not_deny() {
    assert_not_deny(
        "ctlc",
        "exec 3<<< 'chmod 777 /etc/hosts'; exec 0<&3; bash script.sh",
    );
}
