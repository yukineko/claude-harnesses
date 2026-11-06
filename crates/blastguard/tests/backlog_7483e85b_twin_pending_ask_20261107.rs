// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog 7483e85b, twin behaviours of the protected-path-Ask fix (see
//! backlog_protected_path_ask_masks_deny_20261107.rs). In
//! `interpreter_program_verdict` every Ask earned inside the descent must be
//! PENDING (a Deny earned anywhere outranks it), and an unanswered
//! recoverability probe must not reach Allow:
//!  (1) `Recovery::Undetermined` was silently ignored. A relative-path write
//!      program with NO cwd in the payload (no base to resolve against) was
//!      allow; it must be ask (CLAUDE.md section 3).
//!  (2) the `confined_ask` for an unrecoverable write onto a confined root
//!      (an existing untracked file under the temp dir, a safe root) was an
//!      early return, so a later irreversible write to `/etc/hosts` in the same
//!      program was downgraded from deny to ask.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

/// `cwd = None` omits the key from the payload entirely.
fn decision(command: &str, cwd: Option<&str>) -> String {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let mut payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
    });
    if let Some(cwd) = cwd {
        payload["cwd"] = serde_json::json!(cwd);
    }
    let payload = payload.to_string();
    let mut hook = Command::new(bin);
    hook.env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(out.status.code(), Some(0), "hook must exit 0 ({command:?})");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return "allow".to_string();
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout not JSON for {command:?} ({e}): {stdout}"));
    doc.get("hookSpecificOutput")
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no permissionDecision for {command:?}: {stdout}"))
        .to_string()
}

/// An existing, untracked file under the temp dir: unrecoverable (no second
/// copy) yet inside a safe root, so the interpreter write is a confined Ask.
/// Returns (dir, file path).
fn confined_victim(tag: &str) -> (std::path::PathBuf, String) {
    let dir = std::env::temp_dir().join(format!("bg-7483e85b-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("note.txt"), "irreplaceable\n").unwrap();
    let dir = dir.canonicalize().unwrap();
    let file = dir.join("note.txt").to_string_lossy().into_owned();
    (dir, file)
}

// ---------------------------------------------------------------- (1)

#[test]
fn undetermined_probe_relative_write_without_cwd_is_ask() {
    let cmd = "python3 -c \"open('a/b','w')\"";
    assert_eq!(
        decision(cmd, None),
        "ask",
        "a probe that could not answer must not reach Allow: {cmd:?}"
    );
}

#[test]
fn control_relative_write_with_cwd_is_allow() {
    // Same program, base known: the file does not exist, nothing is destroyed.
    let cmd = "python3 -c \"open('a/b','w')\"";
    let cwd = std::env::temp_dir();
    assert_eq!(decision(cmd, cwd.to_str()), "allow", "{cmd:?}");
}

#[test]
fn control_relative_read_without_cwd_is_allow() {
    let cmd = "python3 -c \"print(open('a/b').read())\"";
    assert_eq!(decision(cmd, None), "allow", "{cmd:?}");
}

// ---------------------------------------------------------------- (2)

#[test]
fn control_confined_unrecoverable_write_alone_is_ask() {
    let (dir, file) = confined_victim("alone");
    let cmd = format!("python3 -c \"open('{file}','w').write('')\"");
    let got = decision(&cmd, dir.to_str());
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(got, "ask", "{cmd:?}");
}

#[test]
fn confined_ask_does_not_mask_a_later_deny_write() {
    let (dir, file) = confined_victim("then-deny");
    let cmd =
        format!("python3 -c \"open('{file}','w').write(''); open('/etc/hosts','w').write('')\"");
    let got = decision(&cmd, dir.to_str());
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        got, "deny",
        "a confined Ask must stay pending so the later irreversible write denies: {cmd:?}"
    );
}
