//! Backlog 6798f47d: the stdin-program scan (`python3 - <<'EOF'`) asserts
//! "it runs `rmtree`" when the word appears only in a COMMENT of the program.
//!
//! `INTERPRETER_DESTRUCTIVE_CALLS` (src/detect.rs) matches the bare substring
//! `rmtree`. The same list's own comment calls a reason that "asserts a finding
//! it had not observed" a §4 failure (the `unlink (` case was fixed for that).
//! Measured 2026-10-15 at main e1aca620: Deny with reason "`python3 - <<'EOF'`
//! takes its program from stdin, and it runs `rmtree`, which deletes files or
//! directories outright" for a program that never calls it.
//!
//! Repro: `cargo test -p blastguard --test backlog_6798f47d -- --ignored`

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::json;

fn verdict(command: &str) -> String {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join("backlog_6798f47d");
    let proj = base.join("proj");
    let home = base.join("home");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let payload = json!({
        "hook_event_name": "PreToolUse",
        "cwd": proj.display().to_string(),
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_blastguard"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("CLAUDE_PROJECT_DIR", &proj)
        .env("HOME", &home)
        .env("BLASTGUARD_APPROVALS_DIR", base.join("store"))
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("BLASTGUARD_ASK")
        .env_remove("TMPDIR")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(&base)
        .spawn()
        .expect("binary spawns");
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    assert_eq!(out.status.code(), Some(0));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

const CLAIM: &str = "it runs `rmtree`";

/// Control: a program that REALLY calls rmtree is refused with that reason.
#[test]
fn control_real_rmtree_call_is_refused_with_the_rmtree_reason() {
    let out = verdict("python3 - <<'EOF'\nimport shutil\nshutil.rmtree('/')\nEOF\n");
    assert!(
        out.contains("\"deny\"") && out.contains(CLAIM),
        "a real shutil.rmtree call must be denied naming rmtree; got {out:?}"
    );
}

/// THE DEFECT: the word only in a comment yields a reason claiming the call.
#[test]
#[ignore = "backlog 6798f47d: RED repro"]
fn rmtree_only_in_a_comment_is_not_reported_as_a_call() {
    let cmd = "python3 - <<'EOF'\n# deliberately does not call shutil.rmtree\nprint('ok')\nEOF\n";
    let out = verdict(cmd);
    assert!(
        !out.contains(CLAIM),
        "the program never calls rmtree (it appears only in a comment), so the \
         reason must not assert {CLAIM:?}; got {out:?}"
    );
}
