#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-15, finding F18, relay id
//! bgr-837357910fb9): a worker's
//! `cd <wt> && TASK_ID=$(git config ...) && git add ... && git commit -q -F -
//! --trailer ... <<'EOF' ... EOF` was Ask (Deny in a subagent) with "the command
//! word ``import` is an expansion whose value only exists at run time". The
//! commit message body, inside a QUOTED heredoc, contained a line starting with
//! a backtick (`` `import re; re.compile("x")` ``).
//!
//! Reproduced 2026-11-15 on blastguard 0.2.606 (main d01f71c3d) with the original
//! command verbatim (only the leading cd path swapped to a scratch repo):
//! main = ask, subagent = deny, same reason. Minimal shape (this file):
//!   true && git commit -q -F - <<'EOF'\nsubj\n\n`import re`\nEOF
//! It also fires for `cat <<'EOF'\n`ls`\nEOF\ntrue`. It does NOT fire when the
//! backtick is mid-line (`x `import re``), nor for a bare first-command
//! `git commit -F - <<'EOF'` with the same body, nor for `cd X && git commit`.
//!
//! Two readings: (1) real defect: the body of a quoted heredoc (`<<'EOF'`)
//! undergoes no expansion, so a backtick there is literal text; blastguard
//! treats a column-0 backtick body line as a command word (an expansion) and
//! refuses a command whose heredoc is inert data; (2) intended: a column-0
//! backtick line in a heredoc is deliberately judged as shell, even when the
//! delimiter is quoted. Tell apart: the same body mid-line (`x `import re``) and
//! the same command as a bare first command are allow, so the verdict depends
//! on line position / command position rather than on quoting or on what is
//! executed; and the quoted delimiter makes the body data for `git commit -F -`
//! -> reading (1). The control below shows the restriction is still right where
//! the body really runs (a quoted heredoc fed to `sh`).
//!
//! Asserted: for the minimal commit shape, main == "allow" and subagent ==
//! "allow". Controls: a quoted heredoc fed to `sh` whose body is a backtick `rm`,
//! an unquoted heredoc with a real `` `$T` `` substitution, and a real backtick
//! command word are all != "allow" in main.
//!
//! DOES NOT PROVE: which exact parser path distinguishes `cd X && git commit`
//! and a bare first `git commit` (both allow today) from `true && git commit`
//! (ask), that other interpreters/consumers of `-F -` are handled, that
//! multi-line backtick spans or `$(` at column 0 are handled, or that the
//! original 8-path `git add` command is allowed (only the minimal shape is
//! asserted; the original reproduces by the same mechanism).
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
        "bg-f18-{tag}-{}-{}",
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

/// The minimal reproducing commit shape: not the first command of the line, a
/// QUOTED heredoc, and a body line that starts with a backtick.
const COMMIT_CMD: &str = "true && git commit -q -F - <<'EOF'\nsubj\n\n`import re`\nEOF";

#[test]
#[ignore = "RED: 2026-11-15 F18 quoted-heredoc column-0 backtick body line is judged as a command word (ask / deny in subagent) — remove when fixed"]
fn quoted_heredoc_backtick_line_in_commit_msg_is_allowed() {
    let home = unique_home("fp");
    let (m, mr) = run_as(COMMIT_CMD, &home, false);
    let (s, sr) = run_as(COMMIT_CMD, &home, true);
    assert_eq!(
        m, "allow",
        "main must allow a quoted-heredoc commit message; reason={mr}"
    );
    assert_eq!(
        s, "allow",
        "subagent must allow a quoted-heredoc commit message; reason={sr}"
    );
}

#[test]
fn control_quoted_heredoc_fed_to_sh_with_backtick_rm_is_not_allowed() {
    // The body of a quoted heredoc given to `sh` IS executed: must stay restrictive.
    let home = unique_home("ctl-sh");
    let cmd = "T=hi && sh <<'EOF'\n`rm -rf /tmp/zzz`\nEOF";
    let (m, r) = run_as(cmd, &home, false);
    assert!(m != "allow", "control; cmd={cmd:?} got={m} reason={r}");
}

#[test]
fn control_unquoted_heredoc_real_substitution_is_not_allowed() {
    // Unquoted delimiter: the backtick IS a command substitution of an
    // unresolvable word.
    let home = unique_home("ctl-unq");
    let cmd = "T=hi && git commit -q -F - <<EOF\n`$T`\nEOF";
    let (m, r) = run_as(cmd, &home, false);
    assert!(m != "allow", "control; cmd={cmd:?} got={m} reason={r}");
}

#[test]
fn control_real_backtick_command_word_is_not_allowed() {
    let home = unique_home("ctl-word");
    let cmd = "T=hi && `echo $T` status";
    let (m, r) = run_as(cmd, &home, false);
    assert!(m != "allow", "control; cmd={cmd:?} got={m} reason={r}");
}
