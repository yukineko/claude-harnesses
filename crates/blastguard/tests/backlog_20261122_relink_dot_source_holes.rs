#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Possible fail-opens reported by a blastguard fix agent (2026-11-22, main at
//! 745450fc6, blastguard 0.2.927): a later write `echo x > d/out.txt` is Allow
//! although an earlier segment of the SAME line can plant a symlink to a
//! system path by sourcing (`.` / `source`) code that blastguard cannot see
//! statically, or by a non-shell symlink call:
//!   1. `ln -sf /dev/stdin ~/.cargo/env; . "$HOME/.cargo/env"` (stdin program)
//!   2. `exec 0<<< 'ln -s ...'` then `. /dev/stdin`
//!   3. `cp`/`mv` a file over `~/.cargo/env` then `. "$HOME/.cargo/env"`
//!   4. `python3 -c 'import os; os.symlink(...)'`
//!   5. `. <(echo 'ln -s ...')`, `. -p ...`, `builtin . /dev/stdin <<<`,
//!      `command . /dev/stdin <<<`
//!
//! The sourced code / symlink target is undetermined (or determined as a
//! system path), so the write's real target is undetermined: by CLAUDE.md §3
//! the verdict must not be Allow. This file asserts only NOT Allow (Ask or
//! Deny); it does not pin which of the two.
//!
//! `/etc/bg-sentinel` is a deliberately non-existent sentinel path; nothing in
//! this file runs the commands, they are only fed to the hook as JSON.
//!
//! Fixture: hermetic HOME (with `.cargo/env` present as an ordinary file and
//! `wt/evil.sh` containing `ln -s /etc/bg-sentinel d`), cwd = HOME/wt, main
//! session (CLAUDECODE=1, entrypoint cli). Empty hook stdout = allow.
//!
//! Written by an independent test author (not the implementer). Only shapes
//! observed Allow at authoring time (main c33bde6fd, 2026-11-22) are kept;
//! these were observed NOT Allow and dropped: `exec 0<<< ...; . /dev/stdin`,
//! `. <(echo ...)`, `. -p <(echo ...)`, `source <(echo ...)`.
//! Caveat on s2b: the fixture's `.cargo/env` does not read stdin, so the
//! herestring alone plants nothing; the case shows only that sourcing a
//! file after an `exec 0<<<` program is Allow.
//!
//! DOES NOT PROVE: subagent mode; exactly-Deny; other writers; that the reason
//! text names the relink.

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
        "bg-relink-dot-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    std::fs::create_dir_all(dir.join(".cargo")).unwrap();
    std::fs::write(
        dir.join(".cargo/env"),
        "export PATH=\"$HOME/.cargo/bin:$PATH\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("wt/evil.sh"), "ln -s /etc/bg-sentinel d\n").unwrap();
    Home(dir)
}

fn run_main(command: &str, home: &Home) -> (String, String) {
    let p = serde_json::json!({
        "session_id": "relink-dot-source-holes-20261122",
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

fn assert_not_allow(tag: &str, c: &str) {
    let home = unique_home(tag);
    let (d, r) = run_main(c, &home);
    assert!(
        d == "ask" || d == "deny",
        "write after undetermined dot/source relink must not be allow: {c:?} => {d} ({r})"
    );
}

#[test]
#[ignore = "RED: relink-dot-1 ln -sf /dev/stdin ~/.cargo/env then dot-source"]
fn s1_ln_stdin_over_cargo_env_then_dot_then_write() {
    assert_not_allow(
        "s1",
        "ln -sf /dev/stdin ~/.cargo/env; . \"$HOME/.cargo/env\"; echo x > d/out.txt",
    );
}

#[test]
#[ignore = "RED: relink-dot-2b exec 0<<< then dot cargo env"]
fn s2b_exec_herestring_stdin_then_dot_file_then_write() {
    assert_not_allow(
        "s2b",
        "exec 0<<< 'ln -s /etc/bg-sentinel d/out.txt'; . \"$HOME/.cargo/env\"; echo x > d/out.txt",
    );
}

#[test]
#[ignore = "RED: relink-dot-3 cp over cargo env then dot"]
fn s3_cp_over_cargo_env_then_dot_then_write() {
    assert_not_allow(
        "s3",
        "cp evil.sh ~/.cargo/env; . \"$HOME/.cargo/env\"; echo x > d/out.txt",
    );
}

#[test]
#[ignore = "RED: relink-dot-3b mv over cargo env then dot"]
fn s3b_mv_over_cargo_env_then_dot_then_write() {
    assert_not_allow(
        "s3b",
        "mv evil.sh ~/.cargo/env; . \"$HOME/.cargo/env\"; echo x > d/out.txt",
    );
}

#[test]
#[ignore = "RED: relink-dot-4 python3 -c os.symlink"]
fn s4_python_os_symlink_then_write() {
    assert_not_allow(
        "s4",
        "python3 -c 'import os; os.symlink(\"/etc/bg-sentinel\",\"d\")'; echo x > d/out.txt",
    );
}

#[test]
#[ignore = "RED: relink-dot-5c builtin dot stdin herestring"]
fn s5c_builtin_dot_stdin_herestring_then_write() {
    assert_not_allow(
        "s5c",
        "builtin . /dev/stdin <<< 'ln -s /etc/bg-sentinel d'; echo x > d/out.txt",
    );
}

#[test]
#[ignore = "RED: relink-dot-5d command dot stdin herestring"]
fn s5d_command_dot_stdin_herestring_then_write() {
    assert_not_allow(
        "s5d",
        "command . /dev/stdin <<< 'ln -s /etc/bg-sentinel d'; echo x > d/out.txt",
    );
}
