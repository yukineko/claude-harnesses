#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive introduced by the 3da14bbd fix, blastguard 0.2.617,
//! commit 4b30c0101; observed 2026-11-15 by an independent verifier):
//! on base 9df09a414 each command below was allow in main and subagent mode;
//! on 4b30c0101 it became ask/deny or deny/deny.
//!
//!   git log --oneline | python3 -c 'import os, sys; print(len(sys.stdin.read()))'
//!   cat notes.txt | python3 -c 'import os; print(os.getcwd())'
//!   ls | python3 -c 'import shutil; print(shutil.which("ls"))'
//!   echo 'chmod 777 /etc/hosts' | python3 -c 'import os; print(1)'
//!   echo 'chmod 777 /etc/hosts' | python3 -c 'import os; os.system("ls")'
//!   echo 'chmod 777 /etc/hosts' | python3 -c 'import os; os.system("cat")'
//!   echo 'chmod 777 /etc/hosts' | python3 -c 'import os; os.system("grep chmod")'
//!
//! Suspected mechanism (not asserted here): `child_process_may_read_stdin` in
//! `src/detect/interpreter/python_file.rs` calls `sem::starts_child_process`
//! for every bare Name token, and that matches `RUNNER_MODULES` (`os`,
//! `subprocess`, `shutil`, ...), so a mere `import os` makes the piped stdin
//! "a program".
//!
//! Two readings: (1) real defect: piped stdin is the program only when a
//! child interpreter/shell that reads its program from stdin is actually
//! started; a module import, a `print`, or a literal child command that does
//! not interpret stdin as code (`ls`, `cat`, `grep chmod`) keeps the stdin as
//! data, as the 4b30c0101 commit's own doc says for `os.system("ls")`;
//! (2) intended over-approximation. Tell apart: the user ruling that false
//! positives are bugs, and the commit's own spec ("a literal non-stdin-reading
//! command keeps stdin data") -> reading (1).
//!
//! Asserted: the seven commands above are allow in BOTH main and subagent
//! mode (base behaviour). Controls that must remain deny/deny (or not-allow)
//! on 4b30c0101: a nested REPL `python3 -m asyncio` / `bash` child with
//! harmful stdin, and a non-literal `os.system(c)` whose command cannot be
//! resolved to a non-reading program.
//!
//! DOES NOT PROVE: anything about `subprocess.run` / `os.popen` /
//! `pty.spawn` spellings, other interpreters (`node -e`, `ruby -e`),
//! `python3 -` or `python3 -i`, here-strings / redirects instead of pipes, the
//! reason text, or that the suspected mechanism above is the actual cause.
//! "allow" here means empty hook stdout (no decision emitted); it does not
//! prove which detector paths ran. The non-literal control asserts only
//! not-allow (deny or ask both pass).
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
        "bg-importos-fp-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "importos-fp-20261115",
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

fn assert_allow_both(tag: &str, cmd: &str) {
    let home = unique_home(tag);
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    assert_eq!(
        (m.as_str(), s.as_str()),
        ("allow", "allow"),
        "main/subagent must both allow; cmd={cmd:?} main_reason={mr:?} subagent_reason={sr:?}"
    );
}

fn assert_deny_both(tag: &str, cmd: &str) {
    let home = unique_home(tag);
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    assert_eq!(m, "deny", "main must deny; cmd={cmd:?} reason={mr}");
    assert_eq!(s, "deny", "subagent must deny; cmd={cmd:?} reason={sr}");
}

// ---- false positives (base 9df09a414: allow/allow) ----

#[test]
fn fp_git_log_into_import_os_stdin_reader_is_allow() {
    assert_allow_both(
        "gitlog",
        r#"git log --oneline | python3 -c 'import os, sys; print(len(sys.stdin.read()))'"#,
    );
}

#[test]
fn fp_cat_file_into_import_os_getcwd_is_allow() {
    assert_allow_both(
        "getcwd",
        r#"cat notes.txt | python3 -c 'import os; print(os.getcwd())'"#,
    );
}

#[test]
fn fp_ls_into_import_shutil_which_is_allow() {
    assert_allow_both(
        "which",
        r#"ls | python3 -c 'import shutil; print(shutil.which("ls"))'"#,
    );
}

#[test]
fn fp_shell_text_stdin_into_import_os_print_is_allow() {
    assert_allow_both(
        "print",
        r#"echo 'chmod 777 /etc/hosts' | python3 -c 'import os; print(1)'"#,
    );
}

#[test]
fn fp_shell_text_stdin_into_os_system_ls_is_allow() {
    assert_allow_both(
        "sysls",
        r#"echo 'chmod 777 /etc/hosts' | python3 -c 'import os; os.system("ls")'"#,
    );
}

#[test]
fn fp_shell_text_stdin_into_os_system_cat_is_allow() {
    assert_allow_both(
        "syscat",
        r#"echo 'chmod 777 /etc/hosts' | python3 -c 'import os; os.system("cat")'"#,
    );
}

#[test]
fn fp_shell_text_stdin_into_os_system_grep_is_allow() {
    assert_allow_both(
        "sysgrep",
        r#"echo 'chmod 777 /etc/hosts' | python3 -c 'import os; os.system("grep chmod")'"#,
    );
}

// ---- controls (must stay deny/deny, or not-allow, on 4b30c0101) ----

#[test]
fn control_harmful_python_stdin_into_nested_asyncio_repl_is_deny() {
    assert_deny_both(
        "ctl-asyncio",
        r#"echo 'import os; os.chmod("/etc/hosts", 0o777)' | python3 -c 'import os; os.system("python3 -m asyncio")'"#,
    );
}

#[test]
fn control_harmful_shell_stdin_into_nested_bash_is_deny() {
    assert_deny_both(
        "ctl-bash",
        r#"echo 'chmod 777 /etc/hosts' | python3 -c 'import os; os.system("bash")'"#,
    );
}

#[test]
fn control_non_literal_os_system_child_is_not_allow() {
    let cmd = r#"echo 'import os; os.chmod("/etc/hosts", 0o777)' | python3 -c 'import os; c="python3"; os.system(c)'"#;
    let home = unique_home("ctl-nonlit");
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    assert_ne!(m, "allow", "main must not allow; cmd={cmd:?} reason={mr}");
    assert_ne!(
        s, "allow",
        "subagent must not allow; cmd={cmd:?} reason={sr}"
    );
}
