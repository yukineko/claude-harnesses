#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! S5c: same-command-line import shadowing (user ruling 2026-11-20), pinned
//! by an independent test author (not the implementer). Every test that
//! failed on the base (main 4b83aee6e, blastguard 0.2.747) carries
//! `#[ignore = "RED s5c 2026-11-20: ..."]`; the un-ignored ones passed there
//! and are controls.
//!
//! Ruling: when a command EARLIER on the same command line may write a file
//! `<mod>.py` or a dir `<mod>/` into the directory a later `python3 -c`
//! program runs in, for a module that program imports, the import may be
//! shadowed → not Allow (Ask in main). An earlier write whose target cannot
//! be determined also → Ask.
//!
//! The hook cwd is a fresh tempdir with NO `json.py` / `json/` in it, so the
//! S5b on-disk shadow check (s5b_followups_20261119.rs (a)) cannot fire; only
//! the same-line write can make these not Allow.
//!
//! Nothing here is executed by a shell or an interpreter, only judged by the
//! hook binary. The child env is cleared (`env_clear`), so
//! `CLAUDE_CODE_SESSION_ID` never reaches it.
//!
//! NOT PINNED HERE (open question to the user): the ruling's "earlier write
//! whose target cannot be determined → Ask". On the base, such a write is
//! already not Allow on its own, independent of any later import:
//! `echo hi > "$F"` → deny (undeterminable redirect target), `cp /tmp/x "$F"`
//! → ask. A test of it would not observe S5c, and asserting `ask` for the
//! redirect would require loosening an existing deny.
//!
//! DOES NOT PROVE: reason wording (verdicts only); writers other than `>`
//! redirect, `cp` and `mkdir`; separators other than `;` and `&&`; a `cd`
//! between the write and the program; modules other than `json`; a write into
//! the cwd spelled with an absolute path or `./`; real interpreter behaviour.

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Home(PathBuf);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unique_home(tag: &str) -> Home {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bg-s5c-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// (decision, reason) of the hook for `command` run with hook cwd `cwd`.
fn run_in(command: &str, home: &Home, cwd: &Path, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "s5c-sameline-20261120",
        "cwd": cwd,
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

/// Judge `command` in main and subagent mode with a hook cwd that has no
/// `json.py` / `json/` entry.
fn both_clean_cwd(command: &str, tag: &str) -> ((String, String), (String, String)) {
    let home = unique_home(tag);
    let wt = home.0.join("wt");
    assert!(!wt.join("json.py").exists() && !wt.join("json").exists());
    (
        run_in(command, &home, &wt, false),
        run_in(command, &home, &wt, true),
    )
}

fn assert_ask_main_not_allow_sub(command: &str, tag: &str) {
    let ((m, mr), (s, sr)) = both_clean_cwd(command, tag);
    assert_eq!(
        m, "ask",
        "main must ask; cmd={command:?} got {m} reason={mr}"
    );
    assert_ne!(
        s, "allow",
        "subagent must not allow; cmd={command:?} reason={sr}"
    );
}

fn assert_allow_both(command: &str, tag: &str) {
    let ((m, mr), (s, sr)) = both_clean_cwd(command, tag);
    assert_eq!(m, "allow", "main must allow; cmd={command:?} reason={mr}");
    assert_eq!(
        s, "allow",
        "subagent must allow; cmd={command:?} reason={sr}"
    );
}

// ---------------------------------------------------------------------------
// Ask cases: an earlier write may shadow the later import
// ---------------------------------------------------------------------------

// user ruling 2026-11-20 (S5c): `> json.py` earlier on the line shadows `import json`.
#[test]
fn redirect_writes_json_py_then_import_json_is_ask() {
    assert_ask_main_not_allow_sub(
        "echo 'print(2)' > json.py; python3 -c 'import json; print(1)'",
        "redir",
    );
}

// user ruling 2026-11-20 (S5c): `cp ... json.py &&` shadows `import json`.
#[test]
fn cp_to_json_py_then_import_json_is_ask() {
    assert_ask_main_not_allow_sub("cp /tmp/x json.py && python3 -c 'import json'", "cp");
}

// user ruling 2026-11-20 (S5c): `mkdir json &&` (package dir) shadows `import json`.
#[test]
fn mkdir_json_then_import_json_is_ask() {
    assert_ask_main_not_allow_sub("mkdir json && python3 -c 'import json'", "mkdir");
}

// ---------------------------------------------------------------------------
// Controls: must stay Allow in both modes
// ---------------------------------------------------------------------------

// Control: the earlier write targets a non-module file.
#[test]
fn control_write_notes_txt_then_import_is_allow() {
    assert_allow_both(
        "echo hi > notes.txt; python3 -c 'import json; print(1)'",
        "c-notes",
    );
}

// Control: json.py is written but the program imports nothing.
#[test]
fn control_write_json_py_program_without_import_is_allow() {
    assert_allow_both("echo 'x' > json.py; python3 -c 'print(1)'", "c-noimp");
}

// Control: json.py is written into a DIFFERENT literal dir than the program's
// cwd (the hook cwd).
#[test]
fn control_write_json_py_other_dir_then_import_is_allow() {
    assert_allow_both(
        "echo x > /tmp/bg-s5c-other/json.py; python3 -c 'import json'",
        "c-other",
    );
}
